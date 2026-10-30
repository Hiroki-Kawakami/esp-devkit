/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::format::{IMAGE_DATA_OFFSET, ImageFormat, ImageHeader};

#[derive(Clone, Copy)]
pub struct Image<'a> {
    format: ImageFormat,
    width: u16,
    height: u16,
    stride: usize,
    pixels: &'a [u8],
}

impl Image<'static> {
    /// Reads a resource binary; panics, at compile time for a `static`, when it
    /// is not one.
    pub const fn parse(data: &'static [u8]) -> Self {
        let Some(header) = ImageHeader::read(data) else { panic!("not an image resource") };
        let (_, pixels) = data.split_at(IMAGE_DATA_OFFSET);
        Self {
            format: header.format,
            width: header.width,
            height: header.height,
            stride: header.stride as usize,
            pixels,
        }
    }
}

impl<'a> Image<'a> {
    /// `pixels` must hold `height` rows of `stride` bytes.
    pub fn from_pixels(format: ImageFormat, width: u16, height: u16, stride: usize, pixels: &'a [u8]) -> Self {
        assert!(pixels.len() >= stride * usize::from(height));
        assert!(stride >= usize::from(width) * format.bytes());
        Self { format, width, height, stride, pixels }
    }

    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    pub const fn width(&self) -> u16 {
        self.width
    }

    pub const fn height(&self) -> u16 {
        self.height
    }

    pub const fn stride(&self) -> usize {
        self.stride
    }

    pub const fn pixels(&self) -> &'a [u8] {
        self.pixels
    }
}
