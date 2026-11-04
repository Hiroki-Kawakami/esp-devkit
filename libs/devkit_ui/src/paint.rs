/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Allocator;

use core::time::Duration;

use bsp_rs::display::{self, Blitter, FrameBuffer, PixelFormat, Rotation, Size};
use devkit_gfx::{ArcMask, Argb8888, ClipStore, Color, CornerMask, Corners, DisplayList, GlyphCache, Prepared, Rect, Rgb565, ShadowMask, Surface, draw};
use idf::heap::Caps;

use crate::animate::faded;
use crate::canvas::Canvas;
use crate::node::{NONE, NodeContent, NodeIndex};
use crate::presence::{Snapshot, half};
use crate::style::Shadow;
use crate::theme::Transition;
use crate::tree::{Tree, shadow_extent};
use crate::zoom::{PICTURE_SCALE, Picture, Zoom, scaled};

const BAND_ROWS: i16 = 32;

/// Corner and shadow masks by size; a UI has a handful of each.
pub(crate) struct Effects {
    corners: Vec<CornerMask>,
    rings: Vec<(u8, u8, CornerMask)>,
    shadows: Vec<(u8, u8, ShadowMask)>,
    arcs: Vec<ArcMask>,
    caps: Caps,
}

impl Effects {
    pub(crate) fn new(caps: Caps) -> Self {
        Self { corners: Vec::new(), rings: Vec::new(), shadows: Vec::new(), arcs: Vec::new(), caps }
    }

    pub(crate) fn corner(&mut self, radius: u8) -> CornerMask {
        if let Some(corner) = self.corners.iter().find(|c| c.radius() == u16::from(radius)) {
            return *corner;
        }
        let corner = CornerMask::new_in(u16::from(radius), self.caps);
        self.corners.push(corner);
        corner
    }

    pub(crate) fn ring(&mut self, radius: u8, stroke: u8) -> CornerMask {
        if let Some((_, _, ring)) = self.rings.iter().find(|(r, s, _)| *r == radius && *s == stroke) {
            return *ring;
        }
        let ring = CornerMask::ring_in(u16::from(radius), u16::from(stroke), self.caps);
        self.rings.push((radius, stroke, ring));
        ring
    }

    pub(crate) fn arc(&mut self, radius: u16, stroke: u16) -> ArcMask {
        if let Some(arc) = self.arcs.iter().find(|a| a.radius() == radius && a.stroke() == stroke) {
            return *arc;
        }
        let arc = ArcMask::new_in(radius, stroke, self.caps);
        self.arcs.push(arc);
        arc
    }

    fn shadow(&mut self, blur: u8, radius: u8) -> ShadowMask {
        if let Some((_, _, mask)) = self.shadows.iter().find(|(b, r, _)| *b == blur && *r == radius) {
            return *mask;
        }
        let mask = ShadowMask::new_in(u16::from(blur), u16::from(radius), self.caps);
        self.shadows.push((blur, radius, mask));
        mask
    }
}

/// What a node passes to what it contains.
#[derive(Clone, Copy)]
struct Inherited {
    opacity: u8,
    foreground: Option<Color>,
}

pub(crate) fn dim(color: Color, opacity: u8) -> Color {
    color.with_opacity(opacity)
}

impl Tree {
    /// `(dx, dy)` is the origin of `id`'s parent content on the screen.
    fn paint_node<'a>(&'a self, id: NodeIndex, dx: i32, dy: i32, clip: Rect, inherited: Inherited, effects: &mut Effects, regions: &[Rect], list: &mut DisplayList<'a, Caps>) {
        let node = &self.nodes[id as usize];
        let pose = node.pose.filter(|_| self.capturing.get() != id);
        let (shift_x, shift_y) = match pose {
            Some(_) => node.shift(),
            None => node.translate,
        };
        let x = dx + i32::from(node.rect.x) + i32::from(shift_x);
        let y = dy + i32::from(node.rect.y) + i32::from(shift_y);
        let reach = pose.map_or(0, |p| i32::from(p.reach));
        if y - reach >= i32::from(clip.bottom()) || y + i32::from(node.rect.h) + 64 + reach <= i32::from(clip.y) {
            return;
        }
        let (sx, sy) = (x as i16, y as i16);
        let rect = Rect::new(sx, sy, node.rect.w, node.rect.h);
        let style = &node.style;
        let theme = self.slots[usize::from(node.owner)].theme;
        let palette = theme.palette(self.color_mode);
        let fades = node.fades.as_deref();
        let styled = fades.and_then(|f| f.opacity.as_ref()).map(|fade| fade.opacity(style.opacity.unwrap_or(255))).or(style.opacity);
        let own = node.opacity.or(styled);
        let opacity = own.map_or(inherited.opacity, |o| Color::rgba(0, 0, 0, inherited.opacity).with_opacity(o).a);
        let opacity = pose.map_or(opacity, |p| Color::rgba(0, 0, 0, opacity).with_opacity(p.opacity).a);
        if let Some(pose) = pose
            && pose.scale != 1.0
            && let Some(snapshot) = self.snapshot_of(id)
        {
            let margin = shadow_extent(style);
            let (w, h) = snapshot.size();
            let (sw, sh) = ((f32::from(w) * pose.scale) as i16, (f32::from(h) * pose.scale) as i16);
            let at = (sx - margin + (w - sw) / 2, sy - margin + (h - sh) / 2);
            snapshot.set(pose.scale, opacity);
            list.raster(Rect::new(at.0, at.1, sw, sh).intersect(&clip), at, snapshot);
            return;
        }
        let foreground = faded(fades.and_then(|f| f.foreground.as_ref()), style.foreground.map(|role| palette.color(role)));
        let down = Inherited { opacity, foreground: foreground.or(inherited.foreground) };

        if let Some(Shadow { y: down, blur, color }) = style.shadow {
            let mask = effects.shadow(blur, style.radius);
            let mask = if style.radius == 0 || style.corners == Corners::ALL { mask } else { mask.only(style.corners, effects.shadow(blur, 0)) };
            list.shadow(rect.offset(0, i16::from(down)), mask, dim(palette.color(color), opacity));
        }
        if let Some(fill) = faded(fades.and_then(|f| f.background.as_ref()), style.background.map(|role| palette.color(role))) {
            let color = dim(fill, opacity);
            if style.radius > 0 {
                list.round_rect(rect, effects.corner(style.radius).only(style.corners), color);
            } else {
                list.fill(rect, color);
            }
        }
        let width = style.border_width;
        let outline = faded(fades.and_then(|f| f.border.as_ref()), style.border_color.map(|role| palette.color(role)));
        let border = outline.filter(|_| width > 0).map(|color| ((style.radius > 0).then(|| effects.ring(style.radius, width).only(style.corners)), i16::from(width), dim(color, opacity)));
        let round_clip = style.clip && style.radius > 0 && rect.w.min(rect.h) >= i16::from(style.radius) * 2 && matches!(node.content, NodeContent::Box | NodeContent::Child(_));
        if !round_clip && let Some((ring, width, color)) = border {
            list.border(rect, ring, width, style.border_sides, color);
        }
        if style.scroll_border.is_some_and(|target| self.scrolled_away(node.owner, target)) {
            list.fill(Rect::new(sx, rect.bottom() - 1, rect.w, 1), dim(palette.divider, opacity));
        }

        if id == self.overlay && self.scrim > 0 {
            list.fill(rect.intersect(&clip), Color { a: self.scrim, ..self.scrim_color() });
        }

        let [left, top, right, bottom] = style.padding;
        let (cx, cy) = (x + i32::from(left), y + i32::from(top));
        match &node.content {
            NodeContent::Text(text) => {
                let color = dim(down.foreground.unwrap_or(palette.text), opacity);
                if let Some(edit) = &text.edit {
                    let content = Rect::new(cx as i16, cy as i16, rect.w - left - right, rect.h - top - bottom);
                    let scroll = text.scroll(content.w);
                    list.push_clip(content.intersect(&clip));
                    list.text(cx as i16 - scroll, cy as i16, &text.run, color);
                    let line = text.font.line_height();
                    let at = |byte: u32| text.run.caret(text.shown_byte(byte) as usize).unwrap_or((0, 0));
                    if let Some((from, to)) = edit.underline {
                        let (a, b) = (at(from), at(to));
                        let underline = Rect::new(cx as i16 - scroll + a.0, cy as i16 + a.1 + line - 2, b.0 - a.0, 2);
                        list.fill(underline, dim(palette.color(theme.widgets.text_field.underline_color), opacity));
                    }
                    if self.caret_visible(id) {
                        let (x, y) = at(edit.cursor);
                        list.fill(Rect::new(cx as i16 - scroll + x, cy as i16 + y, 2, line), dim(palette.color(theme.widgets.text_field.caret_color), opacity));
                    }
                    list.pop_clip();
                } else {
                    list.text(cx as i16, cy as i16, &text.run, color);
                }
            }
            NodeContent::Image { image, tint } => list.image(cx as i16, cy as i16, *image, dim(*tint, opacity)),
            NodeContent::External(_) => self.external_split.set(list.ops().len()),
            NodeContent::Progress(progress) => self.paint_progress(progress, rect, style.radius, down.foreground, opacity, palette, effects, list),
            NodeContent::Canvas(canvas) => {
                let viewport = rect.intersect(&clip);
                let Some(host) = &self.slots[usize::from(node.owner)].host else { return };
                list.push_clip(viewport);
                let mut target = Canvas {
                    list,
                    effects,
                    regions,
                    theme: self.slots[usize::from(node.owner)].theme,
                    colors: *palette,
                    origin: (cx as i16, cy as i16),
                    size: (rect.w - left - right, rect.h - top - bottom),
                    opacity,
                    clips: 0,
                };
                unsafe { (canvas.draw.call)(canvas.draw.function, host.component(), &mut target) };
                target.finish();
                list.pop_clip();
            }
            NodeContent::Scroll(scroll) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                self.paint_children(id, cx, cy - scroll.offset, viewport, down, effects, regions, list);
                list.pop_clip();
            }
            NodeContent::Collection(l) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                for &(index, item) in &l.items {
                    let (px, py) = l.grid.place(index as usize);
                    self.paint_node(item, x + px, y + py - l.offset, viewport, down, effects, regions, list);
                }
                list.pop_clip();
            }
            NodeContent::Navigator(nav) if self.transit(id).is_some_and(|t| t.zoom.is_some()) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                let main = nav.main();
                if nav.split.is_some() && main != NONE {
                    self.paint_node(main, x, y, viewport, down, effects, regions, list);
                }
                if let Some(zoom) = self.transit(id).and_then(|t| t.zoom.as_deref()) {
                    list.raster(zoom.area, (zoom.area.x, zoom.area.y), zoom);
                }
                list.pop_clip();
            }
            NodeContent::Navigator(nav) if nav.split.is_some() => {
                let side = nav.split.unwrap_or(0);
                let main = nav.main();
                let viewport = rect.intersect(&clip);
                let right = Rect::new(rect.x + side, rect.y, rect.w - side, rect.h).intersect(&viewport);
                list.push_clip(viewport);
                if main != NONE {
                    self.paint_node(main, x, y, viewport, down, effects, regions, list);
                }
                // Slides stay out of the main screen.
                list.push_clip(right);
                let mut child = node.first_child;
                while child != NONE {
                    if child != main {
                        self.paint_node(child, x, y, right, down, effects, regions, list);
                    }
                    child = self.nodes[child as usize].next;
                }
                list.pop_clip();
                list.pop_clip();
            }
            _ if round_clip => {
                list.push_round_clip(rect, effects.corner(style.radius).only(style.corners));
                self.paint_children(id, x, y, rect.intersect(&clip), down, effects, regions, list);
                list.pop_round_clip();
                // After what is inside, which would cover it.
                if let Some((ring, width, color)) = border {
                    list.border(rect, ring, width, style.border_sides, color);
                }
            }
            _ if style.clip => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                self.paint_children(id, x, y, viewport, down, effects, regions, list);
                list.pop_clip();
            }
            _ => self.paint_children(id, x, y, clip, down, effects, regions, list),
        }
        if self.zoom_split.get().0 == id {
            self.zoom_split.set((id, list.ops().len()));
        }
    }

    fn paint_children<'a>(&'a self, id: NodeIndex, dx: i32, dy: i32, clip: Rect, inherited: Inherited, effects: &mut Effects, regions: &[Rect], list: &mut DisplayList<'a, Caps>) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            self.paint_node(child, dx, dy, clip, inherited, effects, regions, list);
            child = self.nodes[child as usize].next;
        }
    }

    /// `regions` are what the frame repaints.
    pub(crate) fn paint<'a>(&'a self, effects: &mut Effects, regions: &[Rect], list: &mut DisplayList<'a, Caps>) {
        list.reserve(self.nodes.len());
        self.paint_node(self.root, 0, 0, self.area, Inherited { opacity: 255, foreground: None }, effects, regions, list);
    }

    pub(crate) fn zoom_pending(&self) -> bool {
        self.navigators.iter().any(|&nav| self.transit(nav).is_some_and(|t| t.kind == Transition::Zoom && t.zoom.is_none()))
    }

    /// Takes the pictures of zooms starting this frame; without `engine`, on
    /// this task alone. One that gets no pictures changes screens at once.
    pub(crate) fn capture_zooms<C: Allocator + Clone>(&mut self, effects: &mut Effects, cache: &mut GlyphCache<C>, mut engine: Option<&mut Engine<'_>>) {
        for nav in self.navigators.clone() {
            if !self.transit(nav).is_some_and(|t| t.kind == Transition::Zoom && t.zoom.is_none()) {
                continue;
            }
            let Some(zoom) = self.zoom_pictures(nav, effects, cache, engine.as_deref_mut()) else {
                idf::log::warn!(c"ui", "no pictures for a zoom, changing screens at once");
                self.finish_transit(nav);
                continue;
            };
            if let Some(transit) = self.transit_mut(nav) {
                transit.zoom = Some(Box::new(zoom));
                transit.skip_tick = true;
            }
        }
    }

    fn zoom_pictures<C: Allocator + Clone>(&self, nav: NodeIndex, effects: &mut Effects, cache: &mut GlyphCache<C>, mut engine: Option<&mut Engine<'_>>) -> Option<Zoom> {
        let node = &self.nodes[nav as usize];
        let NodeContent::Navigator(navigator) = &node.content else { return None };
        let transit = self.transit(nav)?;
        let side = navigator.split.unwrap_or(0);
        let area = Rect::new(node.screen.x + side, node.screen.y, node.screen.w - side, node.screen.h).intersect(&self.area);
        if area.is_empty() {
            return None;
        }
        let background = self.slots[usize::from(node.owner)].theme.palette(self.color_mode).background;
        let mut entering = Picture::allocate(area)?;
        let mut leaving = Picture::allocate(area)?;
        let shown = self.shown_bands(nav, area, &transit.stale, effects);
        self.take_picture(navigator.shown(), area, background, effects, cache, &mut entering, engine.as_deref_mut(), &[])?;
        self.take_picture(transit.leaving, area, background, effects, cache, &mut leaving, engine, &shown)?;
        Zoom::new(area, entering, leaving, background, transit.forward)
    }

    /// Which bands of `area` the panel shows as the screens of `nav` drew
    /// them: nothing painted over them, and not in `stale`.
    fn shown_bands(&self, nav: NodeIndex, area: Rect, stale: &[Rect], effects: &mut Effects) -> Vec<bool> {
        self.zoom_split.set((nav, usize::MAX));
        let mut list = DisplayList::new_in(self.caps);
        self.paint(effects, &[area], &mut list);
        let (_, split) = self.zoom_split.replace((NONE, 0));
        let mut shown = Vec::new();
        let mut y = 0;
        while y < area.h {
            let band = Rect::new(area.x, area.y + y, area.w, BAND_ROWS.min(area.h - y));
            let covered = split == usize::MAX || devkit_gfx::overlap(list.ops(), split, &band).is_some() || stale.iter().any(|r| r.intersects(&band));
            shown.push(!covered);
            y += BAND_ROWS;
        }
        shown
    }

    /// Bands `shown` comes from the panel through `engine`, the rest is drawn.
    #[allow(clippy::too_many_arguments)]
    fn take_picture<C: Allocator + Clone>(
        &self,
        id: NodeIndex,
        area: Rect,
        background: Color,
        effects: &mut Effects,
        cache: &mut GlyphCache<C>,
        picture: &mut Picture,
        mut engine: Option<&mut Engine<'_>>,
        shown: &[bool],
    ) -> Option<()> {
        let node = &self.nodes[id as usize];
        let dx = i32::from(node.screen.x) - i32::from(node.rect.x) - i32::from(node.translate.0);
        let dy = i32::from(node.screen.y) - i32::from(node.rect.y) - i32::from(node.translate.1);
        let mut list = DisplayList::new_in(self.caps);
        list.fill(area, background);
        self.paint_node(id, dx, dy, area, Inherited { opacity: 255, foreground: None }, effects, &[area], &mut list);
        let mut prepared = Prepared::new_in(self.caps);
        prepared.prepare(&list, &[area], cache);
        let mut clips = ClipStore::new_in(self.caps);
        let width = picture.width as usize;
        let count = width * (BAND_ROWS / 2) as usize;
        let mut bands = [band_memory(count)?, band_memory(count)?];
        let stride = width * size_of::<u16>();
        let target = display::Picture {
            ptr: picture.as_mut_ptr().cast(),
            size: Size { width: picture.width as u16, height: picture.height as u16 },
            format: PixelFormat::Rgb565,
            len: picture.bytes(),
        };
        let (mut y, mut turn) = (0, 0);
        while y < area.h {
            let band = (y / BAND_ROWS) as usize;
            let at = (0, scaled(y) as u16);
            if let Some(engine) = engine.as_deref_mut()
                && let Some((fb, rotation)) = engine.shown
                && shown.get(band).copied().unwrap_or(false)
            {
                let mut end = band + 1;
                while shown.get(end).copied().unwrap_or(false) {
                    end += 1;
                }
                let bottom = (end as i16 * BAND_ROWS).min(area.h);
                let block = display::Rect { x: area.x as u16, y: (area.y + y) as u16, width: area.w as u16, height: (bottom - y) as u16 };
                while engine.blitter.pending() > 1 {
                    wait(engine.blitter);
                }
                if let Err(err) = unsafe { engine.blitter.start_from(&fb, rotation, block, PICTURE_SCALE, target, at) } {
                    idf::log::error!(c"ui", "zoom picture from the panel: {err}");
                    return None;
                }
                y = bottom;
                continue;
            }
            let rows = BAND_ROWS.min(area.h - y);
            let lines = scaled(y + rows) - scaled(y);
            let band = Rect::new(area.x, area.y + y, area.w, rows);
            match engine.as_deref_mut() {
                Some(engine) if lines > 0 => {
                    while engine.blitter.pending() > 1 {
                        wait(engine.blitter);
                    }
                    let memory = &mut bands[turn];
                    let surface = unsafe { Surface::half(memory.as_mut_ptr().cast(), stride, band) };
                    draw::<Rgb565, _, _, _>(&list, &prepared, &mut clips, &surface);
                    let source = display::Picture {
                        ptr: memory.as_mut_ptr().cast(),
                        size: Size { width: width as u16, height: lines as u16 },
                        format: PixelFormat::Rgb565,
                        len: count * size_of::<u16>(),
                    };
                    let block = display::Rect { x: 0, y: 0, width: width as u16, height: lines as u16 };
                    if let Err(err) = unsafe { engine.blitter.start_scaled(source, block, Rotation::R0, 1.0, target, at) } {
                        idf::log::error!(c"ui", "zoom picture: {err}");
                        return None;
                    }
                    turn ^= 1;
                }
                Some(_) => {}
                None => {
                    let memory = unsafe { picture.as_mut_ptr().add(usize::from(at.1) * width) };
                    let surface = unsafe { Surface::half(memory.cast(), stride, band) };
                    draw::<Rgb565, _, _, _>(&list, &prepared, &mut clips, &surface);
                }
            }
            y += rows;
        }
        if let Some(engine) = engine {
            while engine.blitter.pending() > 0 {
                wait(engine.blitter);
            }
        }
        Some(())
    }
}

impl Tree {
    /// `id` as shown at `shown`, without what moves it in or out.
    pub(crate) fn snapshot<C: Allocator + Clone>(&self, id: NodeIndex, shown: Rect, effects: &mut Effects, cache: &mut GlyphCache<C>) -> Option<Snapshot> {
        if shown.is_empty() {
            return None;
        }
        let node = &self.nodes[id as usize];
        let area = shown.expand(shadow_extent(&node.style));
        let dx = i32::from(shown.x) - i32::from(node.rect.x) - i32::from(node.translate.0);
        let dy = i32::from(shown.y) - i32::from(node.rect.y) - i32::from(node.translate.1);
        let mut list = DisplayList::new_in(self.caps);
        self.capturing.set(id);
        self.paint_node(id, dx, dy, area, Inherited { opacity: 255, foreground: None }, effects, &[area], &mut list);
        self.capturing.set(NONE);
        let mut snapshot = Snapshot::allocate(area.w, area.h)?;
        let mut prepared = Prepared::new_in(self.caps);
        prepared.prepare(&list, &[area], cache);
        let mut clips = ClipStore::new_in(self.caps);
        let columns = half(area.w);
        let mut band = argb_band(columns * (BAND_ROWS / 2) as usize)?;
        let mut y = 0;
        while y < area.h {
            let rows = BAND_ROWS.min(area.h - y);
            band.fill(0);
            let surface = unsafe { Surface::half(band.as_mut_ptr().cast(), columns * size_of::<u32>(), Rect::new(area.x, area.y + y, area.w, rows)) };
            draw::<Argb8888, _, _, _>(&list, &prepared, &mut clips, &surface);
            snapshot.store(&band, half(y + rows) - half(y))?;
            y += rows;
        }
        Some(snapshot)
    }
}

fn argb_band(count: usize) -> Option<Vec<u32, Caps>> {
    for caps in [Caps::INTERNAL, Caps::SPIRAM] {
        let mut band = Vec::new_in(caps);
        if band.try_reserve_exact(count).is_ok() {
            band.resize(count, 0);
            return Some(band);
        }
    }
    None
}

/// The 2D engine, and the framebuffer on the panel with its turn.
pub(crate) struct Engine<'a> {
    pub(crate) blitter: &'a mut Blitter,
    pub(crate) shown: Option<(FrameBuffer, Rotation)>,
}

fn band_memory(count: usize) -> Option<Vec<u16, Caps>> {
    for caps in [Caps::DMA | Caps::INTERNAL, Caps::SPIRAM] {
        let mut band = Vec::new_in(caps);
        if band.try_reserve_exact(count).is_ok() {
            band.resize(count, 0);
            return Some(band);
        }
    }
    None
}

/// The pixels stay in use until it returns, so it waits for good.
fn wait(blitter: &mut Blitter) {
    while !blitter.wait_timeout(Duration::from_secs(1)) {
        idf::log::error!(c"ui", "zoom picture stalled");
    }
}
