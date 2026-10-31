/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::ptr;

use crate::color::Color;
use crate::effect::{CornerMask, ShadowMask};
use crate::geom::Rect;
use crate::glyph_cache::GlyphCache;
use crate::list::{DisplayList, MAX_CLIPS, Op};
use crate::pixel::Pixel;
use crate::raster::RasterTarget;
use crate::surface::Surface;
use crate::text::PlacedGlyph;

const NOT_VISIBLE: u32 = u32::MAX;

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
            Op::PushClip(_) => depth = depth.saturating_sub(1),
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

fn draw_text<P: Pixel>(s: &Surface, glyphs: &[PlacedGlyph], masks: &[*const u8], x: i16, y: i16, clip: &Rect, color: Color) {
    for (g, &mask) in glyphs.iter().zip(masks) {
        if !mask.is_null() {
            let rect = Rect::new(g.x + x, g.y + y, i16::from(g.w), i16::from(g.h));
            s.mask::<P>(&rect, clip, mask, isize::from(u8::from(g.w)), color);
        }
    }
}

/// Paints `surface`'s area. Must lie within the regions given to `prepare`.
pub fn draw<P: Pixel, A: Allocator, L: Allocator>(list: &DisplayList<'_, L>, prepared: &Prepared<A>, surface: &Surface) {
    let ops = list.ops();
    let area = surface.area();
    let mut clips = [area; MAX_CLIPS + 1];
    let mut depth = 0;
    for op in &ops[start_op(ops, &area)..] {
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
            Op::Image { x, y, image, tint } => surface.image::<P>(x, y, &image, &clip, tint),
            Op::Text { x, y, glyphs, color, slot, .. } => {
                let start = prepared.starts[slot as usize];
                if start != NOT_VISIBLE {
                    let masks = &prepared.masks[start as usize..start as usize + glyphs.len()];
                    draw_text::<P>(surface, glyphs, masks, x, y, &clip, color);
                }
            }
            Op::Raster { rect, origin, raster } => {
                raster.raster(&mut RasterTarget::new::<P>(surface, rect.intersect(&clip), origin));
            }
            Op::PushClip(_) | Op::PopClip => {}
        }
    }
}
