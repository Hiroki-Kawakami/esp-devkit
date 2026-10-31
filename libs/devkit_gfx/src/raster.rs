/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::color::Color;
use crate::format::ImageFormat;
use crate::geom::Rect;
use crate::pixel::{Argb8888, Bgr888, Pixel, Rgb565};
use crate::surface::Surface;

/// Writes pixels itself. Runs on every task that draws, at the same time, each
/// with its own part of the picture.
pub trait Raster: Sync {
    fn raster(&self, target: &mut RasterTarget<'_>);
}

/// The part of a raster's picture to paint now, in its own coordinates.
pub struct RasterTarget<'a> {
    surface: &'a Surface,
    area: Rect,
    origin: (i16, i16),
    format: ImageFormat,
}

impl<'a> RasterTarget<'a> {
    pub(crate) fn new<P: Pixel>(surface: &'a Surface, area: Rect, origin: (i16, i16)) -> Self {
        Self { surface, area, origin, format: P::NATIVE }
    }

    /// Only this part may be written.
    pub fn area(&self) -> Rect {
        self.area.offset(-self.origin.0, -self.origin.1)
    }

    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// The bytes of `area` on row `y`.
    pub fn row(&mut self, y: i16) -> &mut [u8] {
        let screen = y + self.origin.1;
        assert!(screen >= self.area.y && screen < self.area.bottom());
        let pixels = self.area.w as usize;
        let row = match self.format {
            ImageFormat::Argb8888 => self.surface.bytes::<Argb8888>(self.area.x, screen, pixels),
            ImageFormat::Rgb888 => self.surface.bytes::<Bgr888>(self.area.x, screen, pixels),
            _ => self.surface.bytes::<Rgb565>(self.area.x, screen, pixels),
        };
        unsafe { &mut *row }
    }

    /// Clipped to `area`.
    pub fn fill(&mut self, rect: Rect, color: Color) {
        let rect = rect.offset(self.origin.0, self.origin.1);
        match self.format {
            ImageFormat::Argb8888 => self.surface.fill::<Argb8888>(&rect, &self.area, color),
            ImageFormat::Rgb888 => self.surface.fill::<Bgr888>(&rect, &self.area, color),
            _ => self.surface.fill::<Rgb565>(&rect, &self.area, color),
        }
    }
}
