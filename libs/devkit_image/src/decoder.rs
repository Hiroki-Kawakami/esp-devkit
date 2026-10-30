/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::heap::Caps;

use crate::{BmpDecoder, Error, GifDecoder, ImageInfo, JpegDecoder, PngDecoder, Result, Stream, TiffDecoder, WebpDecoder};

/// Hints a decoder may ignore.
#[derive(Clone, Copy, Default, Debug)]
pub struct DecodeOptions {
    /// Decode-time downscale toward this size, where a format has one: JPEG picks
    /// a 1/N factor. Both must be non-zero to count.
    pub target_width: u16,
    pub target_height: u16,
    /// Bound on the decoded size; past it the decoder downscales further. 0 is
    /// no bound.
    pub max_src_pixels: u32,
    /// Heap for the large buffers; `None` leaves the choice to the decoder.
    pub caps: Option<Caps>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Jpeg,
    Png,
    Bmp,
    Gif,
    Tiff,
    Webp,
}

/// The format of a stream from its first bytes, up to 12.
pub fn sniff(header: &[u8]) -> Option<Format> {
    let starts = |magic: &[u8]| header.starts_with(magic);
    if starts(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Format::Png)
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some(Format::Gif)
    } else if starts(b"II*\0") || starts(b"MM\0*") {
        Some(Format::Tiff)
    } else if header.len() >= 12 && starts(b"RIFF") && &header[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else if starts(b"BM") {
        Some(Format::Bmp)
    } else {
        None
    }
}

/// A stream that cannot seek, with the bytes `open` took off its front put back.
struct Rewound<'a> {
    head: [u8; 12],
    pos: usize,
    len: usize,
    rest: Box<dyn Stream + 'a>,
}

impl Stream for Rewound<'_> {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        if self.pos == self.len {
            return self.rest.read(dst);
        }
        let n = dst.len().min(self.len - self.pos);
        dst[..n].copy_from_slice(&self.head[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Picks the decoder from the stream's first bytes.
pub fn open<'a>(src: impl Stream + 'a, options: &DecodeOptions) -> Result<Decoder<'a>> {
    let mut src: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
    let mut head = [0; 12];
    let mut len = 0;
    while len < head.len() {
        match src.read(&mut head[len..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => len += n,
        }
    }
    let format = sniff(&head[..len]).ok_or(Error::Unsupported)?;
    if src.can_seek() {
        src.seek(0)?;
        Decoder::open(format, src, options)
    } else {
        Decoder::open(format, Rewound { head, pos: 0, len, rest: src }, options)
    }
}

impl<S: Stream + ?Sized> Stream for Box<S> {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        (**self).read(dst)
    }

    fn seek(&mut self, pos: usize) -> Result<()> {
        (**self).seek(pos)
    }

    fn can_seek(&self) -> bool {
        (**self).can_seek()
    }
}

/// Any of the decoders.
pub enum Decoder<'a> {
    Jpeg(JpegDecoder<'a>),
    Png(PngDecoder<'a>),
    Bmp(BmpDecoder<'a>),
    Gif(GifDecoder<'a>),
    Tiff(TiffDecoder<'a>),
    Webp(WebpDecoder<'a>),
}

macro_rules! each {
    ($self:expr, $d:ident => $e:expr) => {
        match $self {
            Self::Jpeg($d) => $e,
            Self::Png($d) => $e,
            Self::Bmp($d) => $e,
            Self::Gif($d) => $e,
            Self::Tiff($d) => $e,
            Self::Webp($d) => $e,
        }
    };
}

impl<'a> Decoder<'a> {
    pub fn open(format: Format, src: impl Stream + 'a, options: &DecodeOptions) -> Result<Self> {
        Ok(match format {
            Format::Jpeg => Self::Jpeg(JpegDecoder::open(src, options)?),
            Format::Png => Self::Png(PngDecoder::open(src, options)?),
            Format::Bmp => Self::Bmp(BmpDecoder::open(src, options)?),
            Format::Gif => Self::Gif(GifDecoder::open(src, options)?),
            Format::Tiff => Self::Tiff(TiffDecoder::open(src, options)?),
            Format::Webp => Self::Webp(WebpDecoder::open(src, options)?),
        })
    }

    pub fn info(&self) -> ImageInfo {
        each!(self, d => d.info())
    }

    /// Writes one row, `info().row_bytes()` long, and returns `true`; `false` at
    /// the end of the image.
    pub fn next_row(&mut self, dst: &mut [u8]) -> Result<bool> {
        each!(self, d => d.next_row(dst))
    }
}
