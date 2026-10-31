/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::alloc::Allocator;

use devkit_gfx::{Color, DisplayList, Image, Raster, Rect, TextRun};
use idf::heap::Caps;

use crate::paint::{Effects, dim};
use crate::theme::Theme;

/// Which `Ui::canvas` of a component is being drawn.
pub type CanvasId = u16;

/// Records a canvas's drawing, in its own coordinates and clipped to it.
pub struct Canvas<'a, 'f> {
    pub(crate) list: &'f mut DisplayList<'a, Caps>,
    pub(crate) effects: &'f mut Effects,
    pub(crate) regions: &'f [Rect],
    pub(crate) theme: &'static Theme,
    pub(crate) origin: (i16, i16),
    pub(crate) size: (i16, i16),
    pub(crate) opacity: u8,
    pub(crate) clips: u8,
}

impl<'a> Canvas<'a, '_> {
    pub fn size(&self) -> (i16, i16) {
        self.size
    }

    pub fn theme(&self) -> &'static Theme {
        self.theme
    }

    fn place(&self, rect: Rect) -> Rect {
        rect.offset(self.origin.0, self.origin.1)
    }

    /// Whether `rect` is repainted this frame. Skipping what is not saves
    /// recording it; the picture is the same either way.
    pub fn needs(&self, rect: Rect) -> bool {
        let rect = self.place(rect);
        self.regions.iter().any(|r| r.intersects(&rect))
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.list.fill(self.place(rect), dim(color, self.opacity));
    }

    pub fn round_rect(&mut self, rect: Rect, radius: u8, color: Color) {
        let rect = self.place(rect);
        let corner = self.effects.corner(radius);
        self.list.round_rect(rect, corner, dim(color, self.opacity));
    }

    pub fn border(&mut self, rect: Rect, radius: u8, stroke: u8, color: Color) {
        let rect = self.place(rect);
        let ring = (radius > 0).then(|| self.effects.ring(radius, stroke));
        self.list.border(rect, ring, i16::from(stroke), dim(color, self.opacity));
    }

    pub fn image(&mut self, x: i16, y: i16, image: Image<'a>, tint: Color) {
        self.list.image(x + self.origin.0, y + self.origin.1, image, dim(tint, self.opacity));
    }

    pub fn text<A: Allocator>(&mut self, x: i16, y: i16, run: &'a TextRun<A>, color: Color) {
        self.list.text(x + self.origin.0, y + self.origin.1, run, dim(color, self.opacity));
    }

    /// `raster` paints `rect` on the drawing tasks, seeing the canvas's
    /// `(0, 0)` as its own. The UI task waits for it, so it may read what
    /// `draw` lent it.
    pub fn raster(&mut self, rect: Rect, raster: &'a dyn Raster) {
        self.list.raster(self.place(rect), self.origin, raster);
    }

    pub fn push_clip(&mut self, rect: Rect) {
        self.list.push_clip(self.place(rect));
        self.clips += 1;
    }

    pub fn pop_clip(&mut self) {
        self.list.pop_clip();
        self.clips -= 1;
    }

    pub(crate) fn finish(&mut self) {
        while self.clips > 0 {
            self.pop_clip();
        }
    }
}
