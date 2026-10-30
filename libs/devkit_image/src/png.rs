/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit_compression::{self as compression, Inflate, Source, Window};
use idf::heap::{self, Caps};

use crate::mem::{Buffer, default_caps, small_buffer, state};
use crate::stream::Reader;
use crate::{DecodeOptions, Error, ImageInfo, PixelFormat, Result, Stream};

const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

struct Idat {
    remaining: u32,
    done: bool,
    buf: [u8; 2048],
    pos: usize,
    len: usize,
}

struct IdatSource<'r, 'a> {
    r: &'r mut Reader<'a>,
    idat: &'r mut Idat,
}

impl Source for IdatSource<'_, '_> {
    fn fill_buf(&mut self) -> compression::Result<&[u8]> {
        let s = &mut *self.idat;
        if s.pos < s.len {
            return Ok(&s.buf[s.pos..s.len]);
        }
        s.pos = 0;
        s.len = 0;
        while s.remaining == 0 {
            if s.done {
                return Ok(&[]);
            }
            let mut hdr = [0; 8];
            let mut tag = [0; 4];
            if !self.r.read_exact(&mut hdr) || !self.r.read_exact(&mut tag) || &tag != b"IDAT" {
                s.done = true;
                return Ok(&[]);
            }
            s.remaining = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        }
        let want = (s.remaining as usize).min(s.buf.len());
        let got = self.r.read(&mut s.buf[..want]);
        s.remaining -= got as u32;
        if got == 0 {
            s.done = true;
        }
        s.len = got;
        Ok(&s.buf[..got])
    }

    fn consume(&mut self, n: usize) {
        self.idat.pos += n;
    }
}

fn composite_white(c: u8, a: u8) -> u8 {
    let (c, a) = (u32::from(c), u32::from(a));
    ((c * a + 255 * (255 - a) + 127) / 255) as u8
}

#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i32::from(a), i32::from(b), i32::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// PNG without interlacing, at up to 8 bits per sample. Alpha is composited over
/// white, gray stays `Gray8`, and everything else comes out `Rgb888`.
pub struct PngDecoder<'a> {
    r: Reader<'a>,
    idat: Box<Idat, Caps>,
    inflate: Box<Inflate, Caps>,
    window: Box<Window, Caps>,

    width: u16,
    height: u16,
    format: PixelFormat,
    bitdepth: u8,
    colortype: u8,
    rowbytes: usize,
    filter_bpp: usize,
    row: u16,

    palette: Box<[[u8; 3]; 256], Caps>,
    trns: Box<[u8; 256], Caps>,
    palette_count: usize,

    prev: Buffer,
    cur: Buffer,
    filtered: Buffer,
}

unsafe impl devkit::Zeroable for Idat {}

impl<'a> PngDecoder<'a> {
    pub fn open(src: impl Stream + 'a, _options: &DecodeOptions) -> Result<Self> {
        let src: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
        let mut r = Reader::new(src)?;

        let mut sig = [0; 8];
        if !r.read_exact(&mut sig) {
            return Err(Error::Truncated);
        }
        if sig != SIGNATURE {
            return Err(Error::Decode);
        }
        let mut lenbuf = [0; 4];
        let mut tag = [0; 4];
        if !r.read_exact(&mut lenbuf) || !r.read_exact(&mut tag) {
            return Err(Error::Truncated);
        }
        if &tag != b"IHDR" || u32::from_be_bytes(lenbuf) != 13 {
            return Err(Error::Decode);
        }
        let mut ih = [0; 13];
        if !r.read_exact(&mut ih) || !r.skip(4) {
            return Err(Error::Truncated);
        }
        let w = u32::from_be_bytes([ih[0], ih[1], ih[2], ih[3]]);
        let h = u32::from_be_bytes([ih[4], ih[5], ih[6], ih[7]]);
        let (bitdepth, colortype) = (ih[8], ih[9]);
        if ih[10] != 0 || ih[11] != 0 {
            return Err(Error::Decode);
        }
        if ih[12] != 0 {
            return Err(Error::Unsupported);
        }
        if w == 0 || h == 0 {
            return Err(Error::Decode);
        }
        if w > 0xFFFF || h > 0xFFFF {
            return Err(Error::TooLarge);
        }
        let low_depth = matches!(bitdepth, 1 | 2 | 4 | 8);
        let (channels, rgb): (usize, bool) = match colortype {
            0 if low_depth => (1, false),
            2 if bitdepth == 8 => (3, true),
            3 if low_depth => (1, true),
            4 if bitdepth == 8 => (2, false),
            6 if bitdepth == 8 => (4, true),
            _ => return Err(Error::Unsupported),
        };

        let mut palette: Box<[[u8; 3]; 256], Caps> = state()?;
        let mut trns: Box<[u8; 256], Caps> = state()?;
        trns.fill(0xFF);
        let mut palette_count = 0;
        let mut idat: Box<Idat, Caps> = state()?;

        loop {
            if !r.read_exact(&mut lenbuf) || !r.read_exact(&mut tag) {
                return Err(Error::Truncated);
            }
            let len = u32::from_be_bytes(lenbuf) as usize;
            match &tag {
                b"PLTE" => {
                    if len % 3 != 0 || len / 3 > 256 {
                        return Err(Error::Decode);
                    }
                    palette_count = len / 3;
                    for entry in &mut palette[..palette_count] {
                        if !r.read_exact(entry) {
                            return Err(Error::Truncated);
                        }
                    }
                    if !r.skip(4) {
                        return Err(Error::Truncated);
                    }
                }
                b"tRNS" => {
                    let k = len.min(256);
                    if !r.read_exact(&mut trns[..k]) || !r.skip(len - k + 4) {
                        return Err(Error::Truncated);
                    }
                }
                b"IDAT" => {
                    idat.remaining = len as u32;
                    break;
                }
                b"IEND" => return Err(Error::Decode),
                _ => {
                    if !r.skip(len.saturating_add(4)) {
                        return Err(Error::Truncated);
                    }
                }
            }
        }
        if colortype == 3 && palette_count == 0 {
            return Err(Error::Decode);
        }

        let window = heap::zeroed_box(default_caps())?;
        let mut inflate: Box<Inflate, Caps> = state()?;
        inflate.start(&mut IdatSource { r: &mut r, idat: &mut idat }).map_err(|_| Error::Decode)?;

        let bits = channels * usize::from(bitdepth);
        let rowbytes = (w as usize * bits).div_ceil(8);
        Ok(Self {
            r,
            idat,
            inflate,
            window,
            width: w as u16,
            height: h as u16,
            format: if rgb { PixelFormat::Rgb888 } else { PixelFormat::Gray8 },
            bitdepth,
            colortype,
            rowbytes,
            filter_bpp: bits.div_ceil(8),
            row: 0,
            palette,
            trns,
            palette_count,
            prev: small_buffer(rowbytes)?,
            cur: small_buffer(rowbytes)?,
            filtered: small_buffer(1 + rowbytes)?,
        })
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo { width: self.width, height: self.height, format: self.format }
    }

    fn unfilter(&mut self) {
        let (bpp, rb) = (self.filter_bpp, self.rowbytes);
        let filter = self.filtered[0];
        let src = self.filtered[1..].as_ptr();
        let prev = self.prev.as_ptr();
        let dst = self.cur.as_mut_ptr();
        // The three buffers hold `rb` bytes each.
        unsafe {
            match filter {
                1 => {
                    core::ptr::copy_nonoverlapping(src, dst, bpp);
                    for i in bpp..rb {
                        *dst.add(i) = (*src.add(i)).wrapping_add(*dst.add(i - bpp));
                    }
                }
                2 => {
                    for i in 0..rb {
                        *dst.add(i) = (*src.add(i)).wrapping_add(*prev.add(i));
                    }
                }
                3 => {
                    for i in 0..bpp {
                        *dst.add(i) = (*src.add(i)).wrapping_add(*prev.add(i) >> 1);
                    }
                    for i in bpp..rb {
                        let avg = (u32::from(*dst.add(i - bpp)) + u32::from(*prev.add(i))) >> 1;
                        *dst.add(i) = (*src.add(i)).wrapping_add(avg as u8);
                    }
                }
                4 => {
                    for i in 0..bpp {
                        *dst.add(i) = (*src.add(i)).wrapping_add(*prev.add(i));
                    }
                    for i in bpp..rb {
                        let p = paeth(*dst.add(i - bpp), *prev.add(i), *prev.add(i - bpp));
                        *dst.add(i) = (*src.add(i)).wrapping_add(p);
                    }
                }
                _ => core::ptr::copy_nonoverlapping(src, dst, rb),
            }
        }
    }

    fn convert_row(&self, dst: &mut [u8]) {
        let line = &self.cur;
        let w = usize::from(self.width);
        let depth = usize::from(self.bitdepth);
        let sample = |x: usize| {
            let bitpos = x * depth;
            usize::from(line[bitpos >> 3] >> (8 - depth - (bitpos & 7))) & ((1 << depth) - 1)
        };
        match self.colortype {
            0 => {
                let maxv = (1 << depth) - 1;
                for (x, out) in dst[..w].iter_mut().enumerate() {
                    *out = (sample(x) * 255 / maxv) as u8;
                }
            }
            2 => dst[..w * 3].copy_from_slice(&line[..w * 3]),
            3 => {
                for (x, out) in dst[..w * 3].chunks_exact_mut(3).enumerate() {
                    let idx = sample(x).min(self.palette_count - 1);
                    let (p, a) = (&self.palette[idx], self.trns[idx]);
                    out[0] = composite_white(p[0], a);
                    out[1] = composite_white(p[1], a);
                    out[2] = composite_white(p[2], a);
                }
            }
            4 => {
                for (out, p) in dst[..w].iter_mut().zip(line.chunks_exact(2)) {
                    *out = composite_white(p[0], p[1]);
                }
            }
            _ => {
                for (out, p) in dst[..w * 3].chunks_exact_mut(3).zip(line.chunks_exact(4)) {
                    out[0] = composite_white(p[0], p[3]);
                    out[1] = composite_white(p[1], p[3]);
                    out[2] = composite_white(p[2], p[3]);
                }
            }
        }
    }

    /// Writes a row and returns `true`; `false` at the end of the image.
    pub fn next_row(&mut self, dst: &mut [u8]) -> Result<bool> {
        if dst.len() < self.info().row_bytes() {
            return Err(Error::InvalidArgument);
        }
        if self.row >= self.height {
            return Ok(false);
        }
        let need = 1 + self.rowbytes;
        let mut src = IdatSource { r: &mut self.r, idat: &mut self.idat };
        match self.inflate.read(&mut src, &mut self.window, &mut self.filtered[..need]) {
            Ok(got) if got == need => {}
            _ => return Err(Error::Truncated),
        }
        self.unfilter();
        self.convert_row(dst);
        core::mem::swap(&mut self.prev, &mut self.cur);
        self.row += 1;
        Ok(true)
    }
}
