/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::ptr;

use crate::color::{Color, mul8};
use crate::effect::{ArcMask, CornerMask, ShadowMask};
use crate::geom::Rect;
use crate::glyph_cache::GlyphCache;
use crate::list::{DisplayList, MAX_CLIPS, Op};
use crate::math::{direction, sqrt};
use crate::pixel::Pixel;
use crate::raster::{Raster, RasterTarget};
use crate::surface::Surface;
use crate::text::PlacedGlyph;

const NOT_VISIBLE: u32 = u32::MAX;

/// What round clips keep while they are open. One per task drawing; it grows
/// to what the frames need and stays.
pub struct ClipStore<A: Allocator = Global> {
    saved: Vec<u8, A>,
}

impl ClipStore<Global> {
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl Default for ClipStore<Global> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator> ClipStore<A> {
    pub fn new_in(alloc: A) -> Self {
        Self { saved: Vec::new_in(alloc) }
    }
}

#[derive(Clone, Copy)]
struct RoundClip {
    rect: Rect,
    corner: CornerMask,
    area: Rect,
    start: usize,
}

impl RoundClip {
    /// Each corner within `area`: the part, and its mask at the part's top left.
    fn corners(&self) -> [(Rect, *const u8, isize); 4] {
        let r = self.corner.radius() as i16;
        let stride = isize::from(r) * 2;
        let data = self.corner.data();
        let last_row = unsafe { data.offset(isize::from(r - 1) * stride) };
        let (rect, right, low) = (self.rect, self.rect.right() - r, self.rect.bottom() - r);
        let place = |x: i16, y: i16, mask: *const u8, mask_stride: isize| {
            let tile = Rect::new(x, y, r, r);
            let part = tile.intersect(&self.area);
            let skip = isize::from(part.y - y) * mask_stride + isize::from(part.x - x);
            (part, unsafe { mask.offset(skip) }, mask_stride)
        };
        [
            place(rect.x, rect.y, data, stride),
            place(right, rect.y, unsafe { data.add(r as usize) }, stride),
            place(rect.x, low, last_row, -stride),
            place(right, low, unsafe { last_row.add(r as usize) }, -stride),
        ]
    }
}

fn open_round_clip<P: Pixel, A: Allocator>(surface: &Surface, clip: RoundClip, store: &mut ClipStore<A>) -> Option<RoundClip> {
    let corners = clip.corners();
    let bytes: usize = corners.iter().filter(|c| !c.0.is_empty()).map(|c| surface.cells(&c.0).area() as usize * P::BYTES).sum();
    if bytes == 0 || store.saved.try_reserve(bytes).is_err() {
        return None;
    }
    for (part, _, _) in corners {
        if !part.is_empty() {
            surface.save::<P, A>(&part, &mut store.saved);
        }
    }
    Some(clip)
}

fn close_round_clip<P: Pixel, A: Allocator>(surface: &Surface, clip: &RoundClip, store: &mut ClipStore<A>) {
    let mut at = clip.start;
    for (part, mask, mask_stride) in clip.corners() {
        if !part.is_empty() {
            surface.restore::<P>(&part, unsafe { store.saved.as_ptr().add(at) }, mask, mask_stride);
            at += surface.cells(&part).area() as usize * P::BYTES;
        }
    }
    store.saved.truncate(clip.start);
}

/// The glyph masks of a frame, resolved by one task so that any number of
/// tasks can then `draw` without locks.
pub struct Prepared<A: Allocator = Global> {
    masks: Vec<*const u8, A>,
    starts: Vec<u32, A>,
    missing: u32,
}

unsafe impl<A: Allocator + Send> Send for Prepared<A> {}
unsafe impl<A: Allocator + Sync> Sync for Prepared<A> {}

impl Prepared<Global> {
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl Default for Prepared<Global> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator + Clone> Prepared<A> {
    pub fn new_in(alloc: A) -> Self {
        Self { masks: Vec::new_in(alloc.clone()), starts: Vec::new_in(alloc), missing: 0 }
    }

    /// Glyphs that did not fit the cache and will not be drawn.
    pub fn missing(&self) -> u32 {
        self.missing
    }

    /// Resolves the glyphs inside `regions`. Masks stay valid until the next
    /// `prepare` on `cache`, and only `regions` may then be drawn.
    pub fn prepare<C: Allocator + Clone, L: Allocator>(
        &mut self,
        list: &DisplayList<'_, L>,
        regions: &[Rect],
        cache: &mut GlyphCache<C>,
    ) {
        cache.begin_frame();
        self.masks.clear();
        self.starts.clear();
        self.starts.resize(list.text_ops() as usize, NOT_VISIBLE);
        self.missing = 0;
        for op in list.ops() {
            let Op::Text { x, y, glyphs, bounds, slot, .. } = *op else { continue };
            if !regions.iter().any(|r| r.intersects(&bounds)) {
                continue;
            }
            self.starts[slot as usize] = self.masks.len() as u32;
            for g in glyphs {
                let rect = Rect::new(g.x + x, g.y + y, i16::from(g.w), i16::from(g.h));
                let mask = if regions.iter().any(|r| r.intersects(&rect)) {
                    let mask = cache.get(g.font, g.glyph);
                    self.missing += u32::from(mask.is_none());
                    mask.unwrap_or(ptr::null())
                } else {
                    ptr::null()
                };
                self.masks.push(mask);
            }
        }
    }
}

fn start_op(ops: &[Op<'_>], area: &Rect) -> usize {
    let mut depth = 0usize;
    for (i, op) in ops.iter().enumerate().rev() {
        match op {
            Op::PopClip => depth += 1,
            Op::PopRoundClip => depth += 1,
            Op::PushClip(_) | Op::PushRoundClip { .. } => depth = depth.saturating_sub(1),
            _ if depth == 0 && op.is_opaque() && op.bounds().contains(area) => return i,
            _ => {}
        }
    }
    0
}

fn draw_round_rect<P: Pixel>(s: &Surface, rect: &Rect, corner: &CornerMask, clip: &Rect, color: Color) {
    let r = corner.radius() as i16;
    let stride = isize::from(r) * 2;
    let data = corner.data();
    let last_row = unsafe { data.offset(isize::from(r - 1) * stride) };
    let right = rect.right() - r;
    let low = rect.bottom() - r;
    s.mask::<P>(&Rect::new(rect.x, rect.y, r, r), clip, data, stride, color);
    s.mask::<P>(&Rect::new(right, rect.y, r, r), clip, unsafe { data.add(r as usize) }, stride, color);
    s.mask::<P>(&Rect::new(rect.x, low, r, r), clip, last_row, -stride, color);
    s.mask::<P>(&Rect::new(right, low, r, r), clip, unsafe { last_row.add(r as usize) }, -stride, color);
    s.fill::<P>(&Rect::new(rect.x + r, rect.y, rect.w - r * 2, r), clip, color);
    s.fill::<P>(&Rect::new(rect.x, rect.y + r, rect.w, rect.h - r * 2), clip, color);
    s.fill::<P>(&Rect::new(rect.x + r, low, rect.w - r * 2, r), clip, color);
}

fn draw_border<P: Pixel>(s: &Surface, rect: &Rect, ring: Option<&CornerMask>, stroke: i16, clip: &Rect, color: Color) {
    let r = ring.map_or(0, |ring| ring.radius() as i16);
    if let Some(ring) = ring {
        let stride = isize::from(r) * 2;
        let data = ring.data();
        let last_row = unsafe { data.offset(isize::from(r - 1) * stride) };
        let (right, low) = (rect.right() - r, rect.bottom() - r);
        s.mask::<P>(&Rect::new(rect.x, rect.y, r, r), clip, data, stride, color);
        s.mask::<P>(&Rect::new(right, rect.y, r, r), clip, unsafe { data.add(r as usize) }, stride, color);
        s.mask::<P>(&Rect::new(rect.x, low, r, r), clip, last_row, -stride, color);
        s.mask::<P>(&Rect::new(right, low, r, r), clip, unsafe { last_row.add(r as usize) }, -stride, color);
    }
    s.fill::<P>(&Rect::new(rect.x + r, rect.y, rect.w - r * 2, stroke), clip, color);
    s.fill::<P>(&Rect::new(rect.x + r, rect.bottom() - stroke, rect.w - r * 2, stroke), clip, color);
    s.fill::<P>(&Rect::new(rect.x, rect.y + r, stroke, rect.h - r * 2), clip, color);
    s.fill::<P>(&Rect::new(rect.right() - stroke, rect.y + r, stroke, rect.h - r * 2), clip, color);
}

fn draw_shadow<P: Pixel>(s: &Surface, rect: &Rect, mask: &ShadowMask, clip: &Rect, color: Color) {
    let outer = rect.expand(mask.blur() as i16);
    let m = mask.margin() as i16;
    let side = mask.side() as isize;
    let data = mask.data();
    let far = isize::from(m) + 1;
    let (right, low) = (outer.right() - m, outer.bottom() - m);
    let at = |x: isize, y: isize| unsafe { data.offset(y * side + x) };
    s.mask::<P>(&Rect::new(outer.x, outer.y, m, m), clip, at(0, 0), side, color);
    s.mask::<P>(&Rect::new(right, outer.y, m, m), clip, at(far, 0), side, color);
    s.mask::<P>(&Rect::new(outer.x, low, m, m), clip, at(0, far), side, color);
    s.mask::<P>(&Rect::new(right, low, m, m), clip, at(far, far), side, color);
    let (w, h) = (outer.w - m * 2, outer.h - m * 2);
    s.mask::<P>(&Rect::new(outer.x, outer.y + m, m, h), clip, at(0, isize::from(m)), 0, color);
    s.mask::<P>(&Rect::new(right, outer.y + m, m, h), clip, at(far, isize::from(m)), 0, color);
    for i in 0..m {
        let u = i as usize;
        let center = mask.margin() as usize;
        let top = color.with_opacity(mask.at(center, u));
        let bottom = color.with_opacity(mask.at(center, center + 1 + u));
        s.fill::<P>(&Rect::new(outer.x + m, outer.y + i, w, 1), clip, top);
        s.fill::<P>(&Rect::new(outer.x + m, low + i, w, 1), clip, bottom);
    }
}

const SPAN: usize = 64;

/// Paints `r` with the color and coverage `pixel` gives for each pixel.
fn draw_pixels<P: Pixel>(s: &Surface, r: &Rect, solid: bool, base: Color, mut pixel: impl FnMut(i16, i16) -> (Color, u8)) {
    let packed = P::pack(base);
    let mut alphas = [0u8; SPAN];
    let cells = s.cells(r);
    for v in cells.y..cells.bottom() {
        let y = s.logical_y(v);
        let mut u = cells.x;
        while u < cells.right() {
            let n = (cells.right() - u).min(SPAN as i16);
            let row = s.at::<P>(u, v);
            for i in 0..n {
                let (color, coverage) = pixel(s.logical_x(u + i), y);
                let a = mul8(color.a, coverage);
                if solid {
                    alphas[i as usize] = a;
                } else if a != 0 {
                    unsafe { P::blend_mask(row.add(i as usize * P::BYTES), &a, 1, P::pack(color)) };
                }
            }
            if solid {
                unsafe { P::blend_mask(row, alphas.as_ptr(), n as usize, packed) };
            }
            u += n;
        }
    }
}

fn same_rgb(a: Color, b: Color) -> bool {
    (a.r, a.g, a.b) == (b.r, b.g, b.b)
}

fn draw_gradient<P: Pixel>(s: &Surface, rect: &Rect, corner: Option<&CornerMask>, from: Color, to: Color, vertical: bool, clip: &Rect) {
    let r = rect.intersect(clip);
    if r.is_empty() {
        return;
    }
    let length = i32::from(if vertical { rect.h } else { rect.w });
    let start = if vertical { rect.y } else { rect.x };
    draw_pixels::<P>(s, &r, same_rgb(from, to), from, |x, y| {
        let at = i32::from(if vertical { y } else { x } - start);
        let color = from.mix(to, ((at * 2 + 1) * 255 / (length * 2)) as u8);
        (color, corner.map_or(255, |corner| corner.coverage(rect, x, y)))
    });
}

fn draw_arc<P: Pixel>(s: &Surface, rect: &Rect, mask: &ArcMask, start: f32, sweep: f32, from: Color, to: Color, clip: &Rect) {
    let r = rect.intersect(clip);
    if r.is_empty() {
        return;
    }
    let whole = sweep >= 360.0;
    let first = start / 360.0;
    let span = (sweep.clamp(0.0, 360.0) / 360.0 * 65536.0) as u32;
    let base = ((first - first as i32 as f32 + 1.0) * 65536.0) as u32 & 0xffff;
    let radius = f32::from(mask.radius());
    let half = f32::from(mask.stroke()) / 2.0;
    let center = radius - half;
    let cap = |t: f32| {
        let (dx, dy) = direction(t);
        (radius + dx * center, radius + dy * center)
    };
    let caps = [cap(first), cap(first + sweep / 360.0)];
    draw_pixels::<P>(s, &r, same_rgb(from, to), from, |x, y| {
        let (lx, ly) = ((x - rect.x) as usize, (y - rect.y) as usize);
        let (coverage, angle) = mask.at(lx, ly);
        if coverage == 0 {
            return (from, 0);
        }
        let past = (u32::from(angle).wrapping_sub(base)) & 0xffff;
        if whole || past <= span {
            let amount = if span == 0 { 0 } else { (past * 255 / span) as u8 };
            return (from.mix(to, amount), coverage);
        }
        let (px, py) = (lx as f32 + 0.5, ly as f32 + 0.5);
        let reach = |(cx, cy): (f32, f32)| {
            let distance = sqrt((px - cx) * (px - cx) + (py - cy) * (py - cy));
            ((half + 0.5 - distance).clamp(0.0, 1.0) * 255.0) as u8
        };
        let (near_start, near_end) = (reach(caps[0]), reach(caps[1]));
        match near_start >= near_end {
            true => (from, near_start.min(coverage)),
            false => (to, near_end.min(coverage)),
        }
    });
}

fn draw_text<P: Pixel>(s: &Surface, glyphs: &[PlacedGlyph], masks: &[*const u8], x: i16, y: i16, clip: &Rect, color: Color) {
    for (g, &mask) in glyphs.iter().zip(masks) {
        if !mask.is_null() {
            let rect = Rect::new(g.x + x, g.y + y, i16::from(g.w), i16::from(g.h));
            s.mask::<P>(&rect, clip, mask, isize::from(u8::from(g.w)), color);
        }
    }
}

/// Rasters the rows a surface at half size samples, at full size a piece at
/// a time over what is below, and keeps the sampled pixels.
fn sample_raster<P: Pixel>(s: &Surface, r: &Rect, origin: (i16, i16), raster: &dyn Raster) {
    let mut line = [0u32; SPAN * 2];
    let line_ptr = line.as_mut_ptr().cast::<u8>();
    let cells = s.cells(r);
    for v in cells.y..cells.bottom() {
        let mut u = cells.x;
        while u < cells.right() {
            let n = (cells.right() - u).min(SPAN as i16);
            let piece = Rect::new(s.logical_x(u), s.logical_y(v), n * 2 - 1, 1);
            let row = s.at::<P>(u, v);
            unsafe {
                for i in 0..n as usize {
                    ptr::copy_nonoverlapping(row.add(i * P::BYTES), line_ptr.add(i * 2 * P::BYTES), P::BYTES);
                }
                let full = Surface::new(line_ptr, SPAN * 2 * P::BYTES, piece);
                raster.raster(&mut RasterTarget::new::<P>(&full, piece, origin));
                for i in 0..n as usize {
                    ptr::copy_nonoverlapping(line_ptr.add(i * 2 * P::BYTES), row.add(i * P::BYTES), P::BYTES);
                }
            }
            u += n;
        }
    }
}

/// Paints `surface`'s area. Must lie within the regions given to `prepare`.
pub fn draw<P: Pixel, A: Allocator, L: Allocator, S: Allocator>(list: &DisplayList<'_, L>, prepared: &Prepared<A>, store: &mut ClipStore<S>, surface: &Surface) {
    let start = start_op(list.ops(), &surface.area());
    draw_ops::<P, A, L, S>(list, start, prepared, store, surface);
}

/// The same with only the operations from `first` on, in the clips open there.
/// Round clips open there stay square: what was under them is not drawn.
pub fn draw_from<P: Pixel, A: Allocator, L: Allocator, S: Allocator>(list: &DisplayList<'_, L>, first: usize, prepared: &Prepared<A>, store: &mut ClipStore<S>, surface: &Surface) {
    draw_ops::<P, A, L, S>(list, first, prepared, store, surface);
}

fn draw_ops<P: Pixel, A: Allocator, L: Allocator, S: Allocator>(list: &DisplayList<'_, L>, first: usize, prepared: &Prepared<A>, store: &mut ClipStore<S>, surface: &Surface) {
    let ops = list.ops();
    let area = surface.area();
    let mut clips = [area; MAX_CLIPS + 1];
    let mut rounds: [Option<RoundClip>; MAX_CLIPS + 1] = [None; MAX_CLIPS + 1];
    let mut depth = 0;
    for (i, op) in ops.iter().enumerate() {
        if i < first {
            match *op {
                Op::PushClip(rect) | Op::PushRoundClip { rect, .. } => {
                    clips[depth + 1] = clips[depth].intersect(&rect);
                    depth += 1;
                }
                Op::PopClip | Op::PopRoundClip => depth -= 1,
                _ => {}
            }
            continue;
        }
        let clip = clips[depth];
        match *op {
            Op::PushClip(rect) => {
                depth += 1;
                clips[depth] = clip.intersect(&rect);
                continue;
            }
            Op::PopClip => {
                depth -= 1;
                continue;
            }
            Op::PushRoundClip { rect, corner } => {
                depth += 1;
                clips[depth] = clip.intersect(&rect);
                rounds[depth] = open_round_clip::<P, S>(surface, RoundClip { rect, corner, area: clips[depth], start: store.saved.len() }, store);
                continue;
            }
            Op::PopRoundClip => {
                if let Some(round) = rounds[depth].take() {
                    close_round_clip::<P, S>(surface, &round, store);
                }
                depth -= 1;
                continue;
            }
            _ => {}
        }
        if !op.bounds().intersects(&clip) {
            continue;
        }
        match *op {
            Op::Fill { rect, color } => surface.fill::<P>(&rect, &clip, color),
            Op::RoundRect { rect, corner, color } => draw_round_rect::<P>(surface, &rect, &corner, &clip, color),
            Op::Border { rect, ring, stroke, color } => draw_border::<P>(surface, &rect, ring.as_ref(), stroke, &clip, color),
            Op::Shadow { rect, mask, color } => draw_shadow::<P>(surface, &rect, &mask, &clip, color),
            Op::Gradient { rect, corner, from, to, vertical } => draw_gradient::<P>(surface, &rect, corner.as_ref(), from, to, vertical, &clip),
            Op::Arc { rect, mask, start, sweep, from, to } => draw_arc::<P>(surface, &rect, &mask, start, sweep, from, to, &clip),
            Op::Image { x, y, image, tint } => surface.image::<P>(x, y, &image, &clip, tint),
            Op::Text { x, y, glyphs, color, slot, .. } => {
                let start = prepared.starts[slot as usize];
                if start != NOT_VISIBLE {
                    let masks = &prepared.masks[start as usize..start as usize + glyphs.len()];
                    draw_text::<P>(surface, glyphs, masks, x, y, &clip, color);
                }
            }
            Op::Raster { rect, origin, raster } if surface.shift() == 0 => {
                raster.raster(&mut RasterTarget::new::<P>(surface, rect.intersect(&clip), origin));
            }
            Op::Raster { rect, origin, raster } => sample_raster::<P>(surface, &rect.intersect(&clip), origin, raster),
            Op::PushClip(_) | Op::PopClip | Op::PushRoundClip { .. } | Op::PopRoundClip => {}
        }
    }
}
