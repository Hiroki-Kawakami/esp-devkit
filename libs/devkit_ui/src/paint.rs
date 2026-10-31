/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_gfx::{Color, CornerMask, DisplayList, Rect, ShadowMask};
use idf::heap::Caps;

use crate::canvas::Canvas;
use crate::node::{NONE, NodeContent, NodeId};
use crate::style::Shadow;
use crate::theme::{Border, Look};
use crate::tree::Tree;

/// Corner and shadow masks by size; a UI has a handful of each.
pub(crate) struct Effects {
    corners: Vec<CornerMask>,
    rings: Vec<(u8, u8, CornerMask)>,
    shadows: Vec<(u8, u8, ShadowMask)>,
    caps: Caps,
}

impl Effects {
    pub(crate) fn new(caps: Caps) -> Self {
        Self { corners: Vec::new(), rings: Vec::new(), shadows: Vec::new(), caps }
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
    fn paint_node<'a>(&'a self, id: NodeId, dx: i32, dy: i32, clip: Rect, inherited: Inherited, effects: &mut Effects, regions: &[Rect], list: &mut DisplayList<'a, Caps>) {
        let node = &self.nodes[id as usize];
        let x = dx + i32::from(node.rect.x) + i32::from(node.translate.0);
        let y = dy + i32::from(node.rect.y) + i32::from(node.translate.1);
        if y >= i32::from(clip.bottom()) || y + i32::from(node.rect.h) + 64 <= i32::from(clip.y) {
            return;
        }
        let (sx, sy) = (x as i16, y as i16);
        let rect = Rect::new(sx, sy, node.rect.w, node.rect.h);
        let style = &node.style;
        let look = style.look.map_or(Look::NONE, |l| l.resolve(node.state));
        let own = node.opacity.or(look.opacity).or(style.opacity);
        let opacity = own.map_or(inherited.opacity, |o| Color::rgba(0, 0, 0, inherited.opacity).with_opacity(o).a);
        let down = Inherited { opacity, foreground: look.foreground.or(inherited.foreground) };

        if let Some(Shadow { y: down, blur, color }) = style.shadow {
            let mask = effects.shadow(blur, style.radius);
            list.shadow(rect.offset(0, i16::from(down)), mask, dim(color, opacity));
        }
        if let Some(color) = look.background.or(style.background) {
            let color = dim(color, opacity);
            if style.radius > 0 {
                list.round_rect(rect, effects.corner(style.radius), color);
            } else {
                list.fill(rect, color);
            }
        }
        if let Some(Border { color, width }) = look.border.or(style.border) {
            let ring = (style.radius > 0).then(|| effects.ring(style.radius, width));
            list.border(rect, ring, i16::from(width), dim(color, opacity));
        }
        if let Some(color) = style.border_top {
            list.fill(Rect::new(sx, sy, rect.w, 1), dim(color, opacity));
        }
        if let Some(color) = style.border_bottom {
            list.fill(Rect::new(sx, rect.bottom() - 1, rect.w, 1), dim(color, opacity));
        }

        let [left, top, right, bottom] = style.padding;
        let (cx, cy) = (x + i32::from(left), y + i32::from(top));
        match &node.content {
            NodeContent::Text(text) => {
                let theme = self.slots[usize::from(node.owner)].theme;
                let color = dim(text.color.or(down.foreground).unwrap_or(theme.colors.on_surface), opacity);
                if let Some(edit) = &text.edit {
                    let content = Rect::new(cx as i16, cy as i16, rect.w - left - right, rect.h - top - bottom);
                    let scroll = text.scroll(content.w);
                    list.push_clip(content.intersect(&clip));
                    list.text(cx as i16 - scroll, cy as i16, &text.run, color);
                    let line = text.font.line_height();
                    let at = |byte: u32| text.run.caret(byte as usize).unwrap_or((0, 0));
                    if let Some((from, to)) = edit.underline {
                        let (a, b) = (at(from), at(to));
                        let underline = Rect::new(cx as i16 - scroll + a.0, cy as i16 + a.1 + line - 2, b.0 - a.0, 2);
                        list.fill(underline, dim(theme.colors.primary, opacity));
                    }
                    if self.caret_visible(id) {
                        let (x, y) = at(edit.cursor);
                        list.fill(Rect::new(cx as i16 - scroll + x, cy as i16 + y, 2, line), dim(theme.colors.primary, opacity));
                    }
                    list.pop_clip();
                } else {
                    list.text(cx as i16, cy as i16, &text.run, color);
                }
            }
            NodeContent::Image { image, tint } => list.image(cx as i16, cy as i16, *image, dim(*tint, opacity)),
            NodeContent::External(_) => self.external_split.set(list.ops().len()),
            NodeContent::Canvas(canvas) => {
                let viewport = rect.intersect(&clip);
                let Some(host) = &self.slots[usize::from(node.owner)].host else { return };
                list.push_clip(viewport);
                let mut target = Canvas {
                    list,
                    effects,
                    regions,
                    theme: self.slots[usize::from(node.owner)].theme,
                    origin: (cx as i16, cy as i16),
                    size: (rect.w - left - right, rect.h - top - bottom),
                    opacity,
                    clips: 0,
                };
                host.draw(canvas.id, &mut target);
                target.finish();
                list.pop_clip();
            }
            NodeContent::Scroll(scroll) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                self.paint_children(id, cx, cy - scroll.offset, viewport, down, effects, regions, list);
                list.pop_clip();
            }
            NodeContent::List(l) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                for &(index, row) in &l.rows {
                    let row_y = cy + index as i32 * i32::from(l.row_height) - l.offset;
                    self.paint_node(row, cx, row_y, viewport, down, effects, regions, list);
                }
                list.pop_clip();
            }
            _ if style.clip => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                self.paint_children(id, x, y, viewport, down, effects, regions, list);
                list.pop_clip();
            }
            _ => self.paint_children(id, x, y, clip, down, effects, regions, list),
        }
    }

    fn paint_children<'a>(&'a self, id: NodeId, dx: i32, dy: i32, clip: Rect, inherited: Inherited, effects: &mut Effects, regions: &[Rect], list: &mut DisplayList<'a, Caps>) {
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
}
