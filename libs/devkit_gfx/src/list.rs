/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::vec::Vec;
use core::alloc::Allocator;

use crate::color::Color;
use crate::effect::{CornerMask, ShadowMask};
use crate::format::ImageFormat;
use crate::geom::Rect;
use crate::image::Image;
use crate::text::{PlacedGlyph, TextRun};

pub(crate) const MAX_CLIPS: usize = 8;

#[derive(Clone, Copy)]
pub enum Op<'a> {
    Fill { rect: Rect, color: Color },
    RoundRect { rect: Rect, corner: CornerMask, color: Color },
    Shadow { rect: Rect, mask: ShadowMask, color: Color },
    Image { x: i16, y: i16, image: Image<'a>, tint: Color },
    Text { x: i16, y: i16, glyphs: &'a [PlacedGlyph], bounds: Rect, color: Color, slot: u32 },
    PushClip(Rect),
    PopClip,
}

impl Op<'_> {
    pub(crate) fn bounds(&self) -> Rect {
        match *self {
            Self::Fill { rect, .. } | Self::RoundRect { rect, .. } => rect,
            Self::Shadow { rect, mask, .. } => rect.expand(mask.blur() as i16),
            Self::Image { x, y, image, .. } => Rect::new(x, y, image.width() as i16, image.height() as i16),
            Self::Text { bounds, .. } => bounds,
            Self::PushClip(rect) => rect,
            Self::PopClip => Rect::default(),
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

/// Drawing operations in painter order.
pub struct DisplayList<'a, A: Allocator = Global> {
    ops: Vec<Op<'a>, A>,
    text_ops: u32,
    clips: usize,
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
        Self { ops: Vec::new_in(alloc), text_ops: 0, clips: 0 }
    }

    pub fn clear(&mut self) {
        self.ops.clear();
        self.text_ops = 0;
        self.clips = 0;
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

    /// Paints only around the middle of `rect`, which something opaque must
    /// cover afterwards.
    pub fn shadow(&mut self, rect: Rect, mask: ShadowMask, color: Color) {
        self.ops.push(Op::Shadow { rect, mask, color });
    }

    /// `tint`'s alpha is the opacity; an `A8` image takes its color too.
    pub fn image(&mut self, x: i16, y: i16, image: Image<'a>, tint: Color) {
        self.ops.push(Op::Image { x, y, image, tint });
    }

    pub fn text<B: Allocator>(&mut self, x: i16, y: i16, run: &'a TextRun<B>, color: Color) {
        let slot = self.text_ops;
        self.text_ops += 1;
        let bounds = run.bounds().offset(x, y);
        self.ops.push(Op::Text { x, y, glyphs: run.glyphs(), bounds, color, slot });
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
}
