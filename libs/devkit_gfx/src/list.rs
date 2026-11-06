/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::vec::Vec;
use core::alloc::Allocator;

use crate::color::Color;
use crate::effect::{ArcMask, CornerMask, ShadowMask};
use crate::format::ImageFormat;
use crate::geom::{Rect, Sides};
use crate::image::Image;
use crate::raster::Raster;
use crate::text::{PlacedGlyph, TextRun};

pub(crate) const MAX_CLIPS: usize = 8;

#[derive(Clone, Copy)]
pub enum Op<'a> {
    Fill { rect: Rect, color: Color },
    RoundRect { rect: Rect, corner: CornerMask, color: Color },
    Border { rect: Rect, ring: Option<CornerMask>, stroke: i16, sides: Sides, color: Color },
    Shadow { rect: Rect, mask: ShadowMask, color: Color },
    Gradient { rect: Rect, corner: Option<CornerMask>, from: Color, to: Color, vertical: bool },
    Arc { rect: Rect, mask: ArcMask, start: f32, sweep: f32, from: Color, to: Color },
    Line { from: (f32, f32), to: (f32, f32), width: f32, color: Color, bounds: Rect },
    Polyline { origin: (i16, i16), points: &'a [(f32, f32)], width: f32, color: Color, bounds: Rect },
    Image { x: i16, y: i16, image: Image<'a>, tint: Color },
    Text { x: i16, y: i16, glyphs: &'a [PlacedGlyph], bounds: Rect, color: Color, slot: u32 },
    Raster { rect: Rect, origin: (i16, i16), raster: &'a dyn Raster },
    PushClip(Rect),
    PopClip,
    PushRoundClip { rect: Rect, corner: CornerMask },
    PopRoundClip,
}

impl Op<'_> {
    pub(crate) fn bounds(&self) -> Rect {
        match *self {
            Self::Fill { rect, .. } | Self::RoundRect { rect, .. } | Self::Border { rect, .. } | Self::Gradient { rect, .. } | Self::Arc { rect, .. } | Self::Raster { rect, .. } => rect,
            Self::Shadow { rect, mask, .. } => rect.expand(mask.blur() as i16),
            Self::Image { x, y, image, .. } => Rect::new(x, y, image.width() as i16, image.height() as i16),
            Self::Text { bounds, .. } | Self::Line { bounds, .. } | Self::Polyline { bounds, .. } => bounds,
            Self::PushClip(rect) | Self::PushRoundClip { rect, .. } => rect,
            Self::PopClip | Self::PopRoundClip => Rect::default(),
        }
    }

    pub(crate) fn is_opaque(&self) -> bool {
        match *self {
            Self::Fill { color, .. } => color.a == 255,
            Self::Image { image, tint, .. } => {
                tint.a == 255 && !matches!(image.format(), ImageFormat::Argb8888 | ImageFormat::A8)
            }
            _ => false,
        }
    }
}

/// The bounding box, within `rect`, of what `ops` from `first` on paints there.
pub fn overlap(ops: &[Op<'_>], first: usize, rect: &Rect) -> Option<Rect> {
    let mut clips = [Rect::new(i16::MIN / 2, i16::MIN / 2, i16::MAX, i16::MAX); MAX_CLIPS + 1];
    let mut depth = 0;
    let mut found: Option<Rect> = None;
    for (i, op) in ops.iter().enumerate() {
        match *op {
            Op::PushClip(clip) | Op::PushRoundClip { rect: clip, .. } => {
                clips[depth + 1] = clips[depth].intersect(&clip);
                depth += 1;
            }
            Op::PopClip | Op::PopRoundClip => depth -= 1,
            _ if i >= first => {
                let part = op.bounds().intersect(&clips[depth]).intersect(rect);
                if !part.is_empty() {
                    found = Some(found.map_or(part, |f| f.union(&part)));
                }
            }
            _ => {}
        }
    }
    found
}


pub struct DisplayList<'a, A: Allocator = Global> {
    ops: Vec<Op<'a>, A>,
    text_ops: u32,
    clips: usize,
    skipped: u32,
}

impl DisplayList<'_, Global> {
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl Default for DisplayList<'_, Global> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, A: Allocator> DisplayList<'a, A> {
    pub fn new_in(alloc: A) -> Self {
        Self { ops: Vec::new_in(alloc), text_ops: 0, clips: 0, skipped: 0 }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.ops.reserve(additional);
    }

    pub fn clear(&mut self) {
        self.ops.clear();
        self.text_ops = 0;
        self.clips = 0;
        self.skipped = 0;
    }

    /// Images left out since `clear` because `draw` cannot take their format.
    pub fn skipped(&self) -> u32 {
        self.skipped
    }

    pub fn ops(&self) -> &[Op<'a>] {
        &self.ops
    }

    pub(crate) fn text_ops(&self) -> u32 {
        self.text_ops
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.ops.push(Op::Fill { rect, color });
    }

    /// `rect` must be at least twice the corner radius on each side.
    pub fn round_rect(&mut self, rect: Rect, corner: CornerMask, color: Color) {
        self.ops.push(Op::RoundRect { rect, corner, color });
    }

    /// An outline `stroke` px wide inside `rect` along `sides`; `ring` rounds
    /// its corners and must have been made for the same stroke.
    pub fn border(&mut self, rect: Rect, ring: Option<CornerMask>, stroke: i16, sides: Sides, color: Color) {
        self.ops.push(Op::Border { rect, ring, stroke, sides, color });
    }

    /// Paints only around the middle of `rect`, which something opaque must
    /// cover afterwards.
    pub fn shadow(&mut self, rect: Rect, mask: ShadowMask, color: Color) {
        self.ops.push(Op::Shadow { rect, mask, color });
    }

    /// `from` at the left, or the top when `vertical`, to `to` at the other
    /// end. `corner` rounds it as `round_rect` does.
    pub fn gradient(&mut self, rect: Rect, corner: Option<CornerMask>, from: Color, to: Color, vertical: bool) {
        self.ops.push(Op::Gradient { rect, corner, from, to, vertical });
    }

    /// Part of `mask`'s ring with round ends, its top left at `(x, y)`: `sweep`
    /// degrees clockwise from `start`, 0 being straight up, going from `from`
    /// to `to`. 360 or more is the whole ring.
    pub fn arc(&mut self, x: i16, y: i16, mask: ArcMask, start: f32, sweep: f32, from: Color, to: Color) {
        let side = mask.radius() as i16 * 2;
        self.ops.push(Op::Arc { rect: Rect::new(x, y, side, side), mask, start, sweep, from, to });
    }

    /// A line `width` px wide with round ends. Pixel `(0, 0)` has its center
    /// at `(0.5, 0.5)`.
    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Color) {
        let bounds = stroke_bounds([from, to].into_iter(), (0, 0), width);
        self.ops.push(Op::Line { from, to, width, color, bounds });
    }

    /// Lines joining `points`, which are relative to `origin`, with round
    /// joins and ends; where they overlap, it is painted once.
    pub fn polyline(&mut self, origin: (i16, i16), points: &'a [(f32, f32)], width: f32, color: Color) {
        let bounds = stroke_bounds(points.iter().copied(), origin, width);
        self.ops.push(Op::Polyline { origin, points, width, color, bounds });
    }

    /// `tint`'s alpha is the opacity; an `A8` image takes its color too.
    pub fn image(&mut self, x: i16, y: i16, image: Image<'a>, tint: Color) {
        if !image.format().is_drawable() {
            self.skipped += 1;
            return;
        }
        self.ops.push(Op::Image { x, y, image, tint });
    }

    pub fn text<B: Allocator>(&mut self, x: i16, y: i16, run: &'a TextRun<B>, color: Color) {
        let slot = self.text_ops;
        self.text_ops += 1;
        let bounds = run.bounds().offset(x, y);
        self.ops.push(Op::Text { x, y, glyphs: run.glyphs(), bounds, color, slot });
    }

    /// `raster` paints `rect`, seeing `origin` as its `(0, 0)`.
    pub fn raster(&mut self, rect: Rect, origin: (i16, i16), raster: &'a dyn Raster) {
        self.ops.push(Op::Raster { rect, origin, raster });
    }

    pub fn push_clip(&mut self, rect: Rect) {
        assert!(self.clips < MAX_CLIPS);
        self.clips += 1;
        self.ops.push(Op::PushClip(rect));
    }

    pub fn pop_clip(&mut self) {
        assert!(self.clips > 0);
        self.clips -= 1;
        self.ops.push(Op::PopClip);
    }

    /// Clips what follows to `rect` with `corner`'s corners. `rect` must be at
    /// least twice the corner radius on each side.
    pub fn push_round_clip(&mut self, rect: Rect, corner: CornerMask) {
        assert!(self.clips < MAX_CLIPS);
        self.clips += 1;
        self.ops.push(Op::PushRoundClip { rect, corner });
    }

    pub fn pop_round_clip(&mut self) {
        assert!(self.clips > 0);
        self.clips -= 1;
        self.ops.push(Op::PopRoundClip);
    }
}

fn stroke_bounds(points: impl Iterator<Item = (f32, f32)>, origin: (i16, i16), width: f32) -> Rect {
    let reach = width / 2.0 + 1.0;
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in points {
        (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
    }
    if left > right {
        return Rect::default();
    }
    let at = |v: f32| v.clamp(-16000.0, 16000.0) as i16;
    let (x, y) = (at(left - reach), at(top - reach));
    Rect::new(x + origin.0, y + origin.1, at(right + reach) + 2 - x, at(bottom + reach) + 2 - y)
}
