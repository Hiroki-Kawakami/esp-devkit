/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod bmp;
mod decoder;
mod dither;
mod encoder;
mod gif;
mod jpeg_decode;
mod jpeg_encode;
mod kernels;
mod mem;
pub mod pipeline;
mod png;
mod raw_encode;
mod recolor;
mod resize;
mod stream;
mod tiff;
mod webp;

use core::fmt;

include!(concat!(env!("OUT_DIR"), "/pie.rs"));

pub use bmp::BmpDecoder;
pub use decoder::{DecodeOptions, Decoder, Format, open, sniff};
pub use dither::{DitherAlgorithm, DitherOptions, DitherOutput, Ditherer, dither_buffer, dither_in_place};
pub use encoder::{Encoder, encode_buffer, encode_stream};
pub use gif::GifDecoder;
pub use jpeg_decode::JpegDecoder;
pub use jpeg_encode::{JpegEncoder, JpegOptions, Subsampling, jpeg_buffer_upper_bound};
pub use mem::{Buffer, default_caps, internal_caps};
pub use png::PngDecoder;
pub use raw_encode::{RawEncoder, RawFormat};
pub use recolor::{GammaIn, GammaOut, Luma, Recolor, RecolorOptions};
pub use resize::{
    Finished, Fit, OrientedFinished, OrientedReady, OrientedResizer, Orientation, Ready, ResizeOptions, ResizedImage,
    Resizer, output_size, resize_buffer, resize_decoder, resize_in_place,
};
pub use stream::{FileStream, Sink, SliceSink, SliceStream, Stream};
pub use tiff::TiffDecoder;
pub use webp::WebpDecoder;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    Truncated,
    Decode,
    Unsupported,
    OutOfMemory,
    InvalidState,
    InvalidArgument,
    TooLarge,
    Io,
}

pub type Result<T> = core::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "Truncated",
            Self::Decode => "DecodeError",
            Self::Unsupported => "UnsupportedFormat",
            Self::OutOfMemory => "OutOfMemory",
            Self::InvalidState => "InvalidState",
            Self::InvalidArgument => "InvalidArgument",
            Self::TooLarge => "TooLarge",
            Self::Io => "IoError",
        })
    }
}

impl From<idf::Error> for Error {
    fn from(_: idf::Error) -> Self {
        Self::OutOfMemory
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    Gray8,
    Rgb888,
    /// Native-endian `u16`; the byte order a panel wants is the panel's concern.
    Rgb565,
    Bgr888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Rgb565 => 2,
            Self::Rgb888 | Self::Bgr888 => 3,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageInfo {
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
}

impl ImageInfo {
    pub const fn row_bytes(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel()
    }
}

#[cfg(test)]
mod tests;
