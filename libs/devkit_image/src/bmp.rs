/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::heap::Caps;

use crate::mem::{Buffer, buffer, state};
use crate::stream::Reader;
use crate::{DecodeOptions, Error, ImageInfo, PixelFormat, Result, Stream};

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

const CHUNK_BYTES: usize = 32768;

#[derive(Clone, Copy, Default)]
struct Channel {
    mask: u32,
    shift: u32,
    bits: u32,
}

impl Channel {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Self::default();
        }
        let shift = mask.trailing_zeros();
        Self { mask, shift, bits: (mask >> shift).trailing_ones() }
    }

    fn value(&self, px: u32) -> u8 {
        if self.bits == 0 {
            return 0;
        }
        let v = (px & self.mask) >> self.shift;
        if self.bits >= 8 { (v >> (self.bits - 8)) as u8 } else { (v * 255 / ((1 << self.bits) - 1)) as u8 }
    }
}

fn le16(p: &[u8]) -> u32 {
    u32::from(u16::from_le_bytes([p[0], p[1]]))
}

fn le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

/// BMP: the core, Windows and V4/V5 headers, 1 to 32 bpp, RLE and bitfields.
/// Rows are stored bottom-up in an ordinary BMP, and read back in chunks from the
/// end, which takes a stream that can seek.
pub struct BmpDecoder<'a> {
    r: Reader<'a>,
    caps: Option<Caps>,

    width: u16,
    height: u16,
    data_offset: usize,
    stride: usize,
    bpp: usize,
    compression: u32,
    bottom_up: bool,
    channels: [Channel; 3],

    palette: Box<[[u8; 3]; 256], Caps>,
    palette_count: usize,

    row: usize,
    chunk: Option<Buffer>,
    chunk_rows: usize,
    chunk_first: usize,
    chunk_count: usize,
    indices: Option<Buffer>,
}

impl<'a> BmpDecoder<'a> {
    pub fn open(src: impl Stream + 'a, options: &DecodeOptions) -> Result<Self> {
        let src: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
        let mut r = Reader::new(src)?;

        let mut fh = [0; 18];
        if !r.read_exact(&mut fh) {
            return Err(Error::Truncated);
        }
        if &fh[..2] != b"BM" {
            return Err(Error::Decode);
        }
        let data_offset = le32(&fh[10..]) as usize;
        let info_size = le32(&fh[14..]) as usize;

        let mut ih = [0; 124];
        let (w, mut h, bpp, compression);
        let mut palette_entry = 4;
        let mut colors_used = 0;
        if info_size == 12 {
            if !r.read_exact(&mut ih[..8]) {
                return Err(Error::Truncated);
            }
            w = le16(&ih) as i32;
            h = le16(&ih[2..]) as i32;
            bpp = le16(&ih[6..]) as usize;
            compression = BI_RGB;
            palette_entry = 3;
        } else if matches!(info_size, 40 | 52 | 56 | 108 | 124) {
            if !r.read_exact(&mut ih[..info_size - 4]) {
                return Err(Error::Truncated);
            }
            w = le32(&ih) as i32;
            h = le32(&ih[4..]) as i32;
            bpp = le16(&ih[10..]) as usize;
            compression = le32(&ih[12..]);
            colors_used = le32(&ih[28..]);
        } else {
            return Err(Error::Unsupported);
        }

        if w <= 0 || h == 0 || h == i32::MIN {
            return Err(Error::Decode);
        }
        let bottom_up = h > 0;
        h = h.abs();
        if w > 0xFFFF || h > 0xFFFF {
            return Err(Error::TooLarge);
        }

        match compression {
            BI_RGB if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) => return Err(Error::Unsupported),
            BI_RGB => {}
            BI_RLE8 if bpp != 8 || !bottom_up => return Err(Error::Decode),
            BI_RLE4 if bpp != 4 || !bottom_up => return Err(Error::Decode),
            BI_RLE8 | BI_RLE4 => {}
            BI_BITFIELDS | BI_ALPHABITFIELDS if bpp != 16 && bpp != 32 => return Err(Error::Decode),
            BI_BITFIELDS | BI_ALPHABITFIELDS => {}
            _ => return Err(Error::Unsupported),
        }

        let mut channels = [Channel::default(); 3];
        if compression == BI_BITFIELDS || compression == BI_ALPHABITFIELDS {
            let mut masks = [0; 12];
            if info_size == 40 {
                if !r.read_exact(&mut masks) {
                    return Err(Error::Truncated);
                }
                if compression == BI_ALPHABITFIELDS && !r.skip(4) {
                    return Err(Error::Truncated);
                }
            } else {
                masks.copy_from_slice(&ih[36..48]);
            }
            for (c, channel) in channels.iter_mut().enumerate() {
                *channel = Channel::new(le32(&masks[4 * c..]));
            }
        } else if bpp == 16 {
            channels = [Channel::new(0x7C00), Channel::new(0x03E0), Channel::new(0x001F)];
        } else if bpp == 32 {
            channels = [Channel::new(0x00FF_0000), Channel::new(0x0000_FF00), Channel::new(0x0000_00FF)];
        }

        let mut palette: Box<[[u8; 3]; 256], Caps> = state()?;
        let mut palette_count = 0;
        if bpp <= 8 {
            let count = if colors_used != 0 { colors_used as usize } else { 1 << bpp };
            palette_count = count.min(256);
            for entry in &mut palette[..palette_count] {
                let mut e = [0; 4];
                if !r.read_exact(&mut e[..palette_entry]) {
                    return Err(Error::Truncated);
                }
                *entry = [e[2], e[1], e[0]];
            }
        }

        let stride = (w as usize * bpp).div_ceil(32) * 4;
        let mut d = Self {
            r,
            caps: options.caps,
            width: w as u16,
            height: h as u16,
            data_offset,
            stride,
            bpp,
            compression,
            bottom_up,
            channels,
            palette,
            palette_count,
            row: 0,
            chunk: None,
            chunk_rows: 0,
            chunk_first: 0,
            chunk_count: 0,
            indices: None,
        };
        if compression == BI_RLE8 || compression == BI_RLE4 {
            return Ok(d);
        }
        if bottom_up && !d.r.can_seek() {
            return Err(Error::Unsupported);
        }
        d.chunk_rows = (CHUNK_BYTES / stride).clamp(1, h as usize);
        d.chunk = Some(buffer(d.chunk_rows * stride, options.caps)?);
        Ok(d)
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo { width: self.width, height: self.height, format: PixelFormat::Rgb888 }
    }

    fn put_index(&self, dst: &mut [u8], idx: usize) {
        let idx = if idx >= self.palette_count { 0 } else { idx };
        dst[..3].copy_from_slice(&self.palette[idx]);
    }

    fn convert_row(&self, src: &[u8], dst: &mut [u8]) {
        let w = usize::from(self.width);
        match self.bpp {
            1 | 4 | 8 => {
                let bpp = self.bpp;
                for (x, out) in dst[..w * 3].chunks_exact_mut(3).enumerate() {
                    let bitpos = x * bpp;
                    let idx = usize::from(src[bitpos >> 3] >> (8 - bpp - (bitpos & 7))) & ((1 << bpp) - 1);
                    self.put_index(out, idx);
                }
            }
            16 => {
                for (out, p) in dst[..w * 3].chunks_exact_mut(3).zip(src.chunks_exact(2)) {
                    let px = le16(p);
                    for c in 0..3 {
                        out[c] = self.channels[c].value(px);
                    }
                }
            }
            24 => {
                for (out, p) in dst[..w * 3].chunks_exact_mut(3).zip(src.chunks_exact(3)) {
                    out[0] = p[2];
                    out[1] = p[1];
                    out[2] = p[0];
                }
            }
            _ => {
                for (out, p) in dst[..w * 3].chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                    let px = le32(p);
                    for c in 0..3 {
                        out[c] = self.channels[c].value(px);
                    }
                }
            }
        }
    }

    fn load_chunk(&mut self, stored: usize) -> bool {
        let h = usize::from(self.height);
        let mut first = stored;
        let mut count = self.chunk_rows;
        if self.bottom_up {
            first = (stored + 1).saturating_sub(count);
            count = stored - first + 1;
        } else if first + count > h {
            count = h - first;
        }
        let at = self.data_offset + first * self.stride;
        if !self.r.seek(at) {
            return false;
        }
        let chunk = self.chunk.as_mut().unwrap();
        if !self.r.read_exact(&mut chunk[..count * self.stride]) {
            return false;
        }
        self.chunk_first = first;
        self.chunk_count = count;
        true
    }

    fn expand_rle(&mut self) -> Result<()> {
        let (w, h) = (usize::from(self.width), usize::from(self.height));
        let mut indices: Buffer = buffer(w * h, self.caps)?;
        if !self.r.seek(self.data_offset) {
            return Err(Error::Truncated);
        }
        let four = self.compression == BI_RLE4;
        let r = &mut self.r;
        let (mut x, mut y) = (0, 0);
        let mut put = |x: &mut usize, y: usize, idx: u8| {
            if *x < w && y < h {
                indices[y * w + *x] = idx;
            }
            *x += 1;
        };
        while y < h {
            let (Some(count), Some(value)) = (r.byte(), r.byte()) else {
                return Err(Error::Truncated);
            };
            if count > 0 {
                for i in 0..count {
                    let idx = if !four {
                        value
                    } else if i & 1 != 0 {
                        value & 0x0F
                    } else {
                        value >> 4
                    };
                    put(&mut x, y, idx);
                }
            } else if value == 0 {
                x = 0;
                y += 1;
            } else if value == 1 {
                break;
            } else if value == 2 {
                let (Some(dx), Some(dy)) = (r.byte(), r.byte()) else {
                    return Err(Error::Truncated);
                };
                x += usize::from(dx);
                y += usize::from(dy);
            } else {
                let bytes = if four { usize::from(value).div_ceil(2) } else { usize::from(value) };
                let mut b = 0;
                for i in 0..value {
                    if !four || i & 1 == 0 {
                        b = r.byte().ok_or(Error::Truncated)?;
                    }
                    let idx = if !four {
                        b
                    } else if i & 1 != 0 {
                        b & 0x0F
                    } else {
                        b >> 4
                    };
                    put(&mut x, y, idx);
                }
                if bytes & 1 != 0 && r.byte().is_none() {
                    return Err(Error::Truncated);
                }
            }
        }
        self.indices = Some(indices);
        Ok(())
    }

    /// Writes a row and returns `true`; `false` at the end of the image.
    pub fn next_row(&mut self, dst: &mut [u8]) -> Result<bool> {
        if dst.len() < self.info().row_bytes() {
            return Err(Error::InvalidArgument);
        }
        let h = usize::from(self.height);
        if self.row >= h {
            return Ok(false);
        }
        let stored = if self.bottom_up { h - 1 - self.row } else { self.row };

        if self.compression == BI_RLE8 || self.compression == BI_RLE4 {
            if self.indices.is_none() {
                self.expand_rle()?;
            }
            let w = usize::from(self.width);
            let line = &self.indices.as_ref().unwrap()[stored * w..(stored + 1) * w];
            for (out, &idx) in dst.chunks_exact_mut(3).zip(line) {
                self.put_index(out, usize::from(idx));
            }
            self.row += 1;
            return Ok(true);
        }

        if self.chunk_count == 0
            || stored < self.chunk_first
            || stored >= self.chunk_first + self.chunk_count
        {
            if !self.load_chunk(stored) {
                return Err(Error::Truncated);
            }
        }
        let at = (stored - self.chunk_first) * self.stride;
        let chunk = self.chunk.as_ref().unwrap();
        self.convert_row(&chunk[at..at + self.stride], dst);
        self.row += 1;
        Ok(true)
    }
}
