/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::{Encoder, Error, ImageInfo, PixelFormat, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RawFormat {
    /// 8 bpp gray.
    L8,
    /// 4 bpp indexed from `Gray8` indices, first pixel in the high nibble.
    I4,
    /// 1 bpp indexed from `Gray8` indices, first pixel in the high bit.
    I1,
    Rgb888,
    Rgb565,
}

impl RawFormat {
    pub const fn stride(self, width: u16) -> usize {
        let width = width as usize;
        match self {
            Self::L8 => width,
            Self::I4 => width.div_ceil(2),
            Self::I1 => width.div_ceil(8),
            Self::Rgb888 => width * 3,
            Self::Rgb565 => width * 2,
        }
    }

    pub const fn input_format(self) -> PixelFormat {
        match self {
            Self::L8 | Self::I4 | Self::I1 => PixelFormat::Gray8,
            Self::Rgb888 => PixelFormat::Rgb888,
            Self::Rgb565 => PixelFormat::Rgb565,
        }
    }
}

/// Packs rows into a buffer at a fixed stride.
pub struct RawEncoder<'a> {
    format: RawFormat,
    width: u16,
    height: u16,
    stride: usize,
    dst: &'a mut [u8],
    row: u16,
}

impl<'a> RawEncoder<'a> {
    pub fn new(format: RawFormat, width: u16, height: u16, dst: &'a mut [u8]) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidArgument);
        }
        let stride = format.stride(width);
        if dst.len() < stride * usize::from(height) {
            return Err(Error::InvalidArgument);
        }
        Ok(Self { format, width, height, stride, dst, row: 0 })
    }

    pub const fn stride(&self) -> usize {
        self.stride
    }

    pub const fn buffer_size(&self) -> usize {
        self.stride * self.height as usize
    }
}

fn pack_i4(dst: &mut [u8], src: &[u8]) {
    for (out, pair) in dst.iter_mut().zip(src.chunks(2)) {
        let lo = pair.get(1).map_or(0, |v| v & 0x0F);
        *out = ((pair[0] & 0x0F) << 4) | lo;
    }
}

fn pack_i1(dst: &mut [u8], src: &[u8]) {
    for (out, bits) in dst.iter_mut().zip(src.chunks(8)) {
        *out = bits.iter().enumerate().fold(0, |b, (k, v)| if v & 1 != 0 { b | (0x80 >> k) } else { b });
    }
}

impl Encoder for RawEncoder<'_> {
    type Output = usize;

    fn info(&self) -> ImageInfo {
        ImageInfo { width: self.width, height: self.height, format: self.format.input_format() }
    }

    fn push_row(&mut self, row: &[u8]) -> Result<()> {
        if self.row >= self.height {
            return Err(Error::InvalidState);
        }
        let input = self.info().row_bytes();
        let src = row.get(..input).ok_or(Error::InvalidArgument)?;
        let at = usize::from(self.row) * self.stride;
        let dst = &mut self.dst[at..at + self.stride];
        match self.format {
            RawFormat::I4 => pack_i4(dst, src),
            RawFormat::I1 => pack_i1(dst, src),
            RawFormat::L8 | RawFormat::Rgb888 | RawFormat::Rgb565 => dst.copy_from_slice(src),
        }
        self.row += 1;
        Ok(())
    }

    fn finish(self) -> Result<usize> {
        if self.row != self.height {
            return Err(Error::InvalidState);
        }
        Ok(self.buffer_size())
    }
}
