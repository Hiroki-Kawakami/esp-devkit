/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit::Zeroable;
use devkit_compression::{self as compression, Error as CompressionError, Lzw, LzwOptions, LzwWork, Source};
use idf::heap::{self, Caps};

use crate::mem::{Buffer, buffer, default_caps, state};
use crate::stream::Reader;
use crate::{DecodeOptions, Error, ImageInfo, PixelFormat, Result, Stream};

struct Blocks {
    done: bool,
    buf: [u8; 255],
    pos: usize,
    len: usize,
}

unsafe impl Zeroable for Blocks {}

struct BlockSource<'r, 'a> {
    r: &'r mut Reader<'a>,
    blocks: &'r mut Blocks,
}

impl Source for BlockSource<'_, '_> {
    fn fill_buf(&mut self) -> compression::Result<&[u8]> {
        let s = &mut *self.blocks;
        if s.pos < s.len {
            return Ok(&s.buf[s.pos..s.len]);
        }
        s.pos = 0;
        s.len = 0;
        if s.done {
            return Ok(&[]);
        }
        let len = match self.r.byte() {
            None | Some(0) => {
                s.done = true;
                return Ok(&[]);
            }
            Some(len) => usize::from(len),
        };
        s.len = self.r.read(&mut s.buf[..len]);
        if s.len < len {
            s.done = true;
        }
        Ok(&s.buf[..s.len])
    }

    fn consume(&mut self, n: usize) {
        self.blocks.pos += n;
    }
}

fn le16(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}

fn skip_blocks(r: &mut Reader) -> bool {
    loop {
        match r.byte() {
            None => return false,
            Some(0) => return true,
            Some(len) => {
                if !r.skip(usize::from(len)) {
                    return false;
                }
            }
        }
    }
}

fn read_palette(r: &mut Reader, palette: &mut [[u8; 3]; 256], flags: u8) -> Option<usize> {
    let count = 2usize << (flags & 7);
    for entry in &mut palette[..count] {
        if !r.read_exact(entry) {
            return None;
        }
    }
    Some(count)
}

fn lzw_error(e: CompressionError) -> Error {
    if e == CompressionError::Corrupt { Error::Decode } else { Error::Truncated }
}

fn read_frame_row(src: &mut dyn Source, lzw: &mut Lzw, work: &mut LzwWork, dst: &mut [u8]) -> Result<()> {
    match lzw.read(src, work, dst) {
        Ok(n) if n == dst.len() => Ok(()),
        Ok(_) => Err(Error::Truncated),
        Err(e) => Err(lzw_error(e)),
    }
}

/// The first frame of a GIF on a white canvas, interlaced or not.
pub struct GifDecoder<'a> {
    r: Reader<'a>,
    caps: Option<Caps>,

    blocks: Box<Blocks, Caps>,
    lzw: Box<Lzw, Caps>,
    work: Box<LzwWork, Caps>,

    palette: Box<[[u8; 3]; 256], Caps>,
    palette_count: usize,
    transparent: Option<u8>,

    width: u16,
    height: u16,
    left: usize,
    top: usize,
    frame_w: usize,
    frame_h: usize,
    interlaced: bool,

    row: usize,
    line: Buffer,
    indices: Option<Buffer>,
}

impl<'a> GifDecoder<'a> {
    pub fn open(src: impl Stream + 'a, options: &DecodeOptions) -> Result<Self> {
        let src: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
        let mut r = Reader::new(src)?;

        let mut hdr = [0; 13];
        if !r.read_exact(&mut hdr) {
            return Err(Error::Truncated);
        }
        if &hdr[..6] != b"GIF87a" && &hdr[..6] != b"GIF89a" {
            return Err(Error::Decode);
        }
        let mut w = u32::from(le16(&hdr[6..]));
        let mut h = u32::from(le16(&hdr[8..]));
        let mut palette: Box<[[u8; 3]; 256], Caps> = state()?;
        let mut palette_count = 0;
        if hdr[10] & 0x80 != 0 {
            palette_count = read_palette(&mut r, &mut palette, hdr[10]).ok_or(Error::Truncated)?;
        }

        let mut transparent = None;
        loop {
            let tag = r.byte().ok_or(Error::Truncated)?;
            if tag == 0x2C {
                break;
            }
            if tag != 0x21 {
                return Err(Error::Decode);
            }
            let label = r.byte().ok_or(Error::Truncated)?;
            if label == 0xF9 {
                let mut gce = [0; 255];
                let len = usize::from(r.byte().ok_or(Error::Truncated)?);
                if !r.read_exact(&mut gce[..len]) {
                    return Err(Error::Truncated);
                }
                transparent = if len >= 4 && gce[0] & 1 != 0 { Some(gce[3]) } else { None };
            }
            if !skip_blocks(&mut r) {
                return Err(Error::Truncated);
            }
        }

        let mut desc = [0; 9];
        if !r.read_exact(&mut desc) {
            return Err(Error::Truncated);
        }
        let left = u32::from(le16(&desc));
        let top = u32::from(le16(&desc[2..]));
        let frame_w = u32::from(le16(&desc[4..]));
        let frame_h = u32::from(le16(&desc[6..]));
        let interlaced = desc[8] & 0x40 != 0;
        if desc[8] & 0x80 != 0 {
            palette_count = read_palette(&mut r, &mut palette, desc[8]).ok_or(Error::Truncated)?;
        }
        if palette_count == 0 {
            for (i, entry) in palette.iter_mut().enumerate() {
                *entry = [i as u8; 3];
            }
            palette_count = 256;
        }

        w = w.max(left + frame_w);
        h = h.max(top + frame_h);
        if w == 0 || h == 0 {
            return Err(Error::Decode);
        }
        if w > 0xFFFF || h > 0xFFFF {
            return Err(Error::TooLarge);
        }

        let min_bits = r.byte().ok_or(Error::Truncated)?;
        if !(2..=8).contains(&min_bits) {
            return Err(Error::Decode);
        }

        let mut lzw: Box<Lzw, Caps> = state()?;
        let mut work: Box<LzwWork, Caps> = heap::zeroed_box(default_caps())?;
        let lzw_options = LzwOptions { literal_bits: min_bits, msb_first: false, early_change: false };
        lzw.start(&lzw_options, &mut work).map_err(|_| Error::Decode)?;

        Ok(Self {
            r,
            caps: options.caps,
            blocks: state()?,
            lzw,
            work,
            palette,
            palette_count,
            transparent,
            width: w as u16,
            height: h as u16,
            left: left as usize,
            top: top as usize,
            frame_w: frame_w as usize,
            frame_h: frame_h as usize,
            interlaced,
            row: 0,
            line: buffer((frame_w as usize).max(1), None)?,
            indices: None,
        })
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo { width: self.width, height: self.height, format: PixelFormat::Rgb888 }
    }

    fn expand_interlaced(&mut self) -> Result<()> {
        const START: [usize; 4] = [0, 4, 2, 1];
        const STEP: [usize; 4] = [8, 8, 4, 2];
        let (w, h) = (self.frame_w, self.frame_h);
        let mut indices: Buffer = buffer(w * h, self.caps)?;
        for pass in 0..4 {
            for y in (START[pass]..h).step_by(STEP[pass]) {
                let mut src = BlockSource { r: &mut self.r, blocks: &mut self.blocks };
                read_frame_row(&mut src, &mut self.lzw, &mut self.work, &mut indices[y * w..(y + 1) * w])?;
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
        if self.row >= usize::from(self.height) {
            return Ok(false);
        }
        let y = self.row;
        dst[..usize::from(self.width) * 3].fill(0xFF);

        if y >= self.top && y < self.top + self.frame_h {
            let fy = y - self.top;
            let w = self.frame_w;
            if self.interlaced {
                if self.indices.is_none() {
                    self.expand_interlaced()?;
                }
            } else {
                let mut src = BlockSource { r: &mut self.r, blocks: &mut self.blocks };
                read_frame_row(&mut src, &mut self.lzw, &mut self.work, &mut self.line[..w])?;
            }
            let line = match &self.indices {
                Some(indices) => &indices[fy * w..(fy + 1) * w],
                None => &self.line[..w],
            };
            for (out, &idx) in dst[self.left * 3..].chunks_exact_mut(3).zip(line) {
                if Some(idx) == self.transparent {
                    continue;
                }
                let idx = if usize::from(idx) >= self.palette_count { 0 } else { usize::from(idx) };
                out.copy_from_slice(&self.palette[idx]);
            }
        }
        self.row += 1;
        Ok(true)
    }
}
