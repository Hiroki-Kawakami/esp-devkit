/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod color;
mod damage;
mod effect;
mod font;
pub mod format;
mod geom;
mod glyph_cache;
mod image;
mod list;
mod pixel;
mod raster;
mod render;
mod surface;
mod text;

pub use color::Color;
pub use damage::Damage;
pub use effect::{CornerMask, ShadowMask};
pub use font::Font;
pub use format::ImageFormat;
pub use geom::Rect;
pub use glyph_cache::GlyphCache;
pub use image::Image;
pub use list::{DisplayList, Op, overlap};
pub use pixel::{Argb8888, Bgr888, Pixel, Rgb565};
pub use raster::{Raster, RasterTarget};
pub use render::{Prepared, draw, draw_from};
pub use surface::Surface;
pub use text::{Align, FontChain, Overflow, PlacedGlyph, TextOptions, TextRun};

#[cfg(test)]
mod tests;
