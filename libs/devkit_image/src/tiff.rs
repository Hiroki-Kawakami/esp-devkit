/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;

use devkit::Zeroable;
use devkit_compression::{self as compression, Inflate, Lzw, LzwOptions, LzwWork, Source, Window};
use idf::heap::{self, Caps};

use crate::mem::{Buffer, buffer, default_caps, state};
use crate::stream::Reader;
use crate::{DecodeOptions, Error, ImageInfo, PixelFormat, Result, Stream};

const COMP_NONE: u16 = 1;
const COMP_LZW: u16 = 5;
const COMP_DEFLATE: u16 = 8;
const COMP_DEFLATE_OLD: u16 = 32946;
const COMP_PACKBITS: u16 = 32773;

const PHOTO_WHITE_IS_ZERO: u16 = 0;
const PHOTO_BLACK_IS_ZERO: u16 = 1;
const PHOTO_RGB: u16 = 2;
const PHOTO_PALETTE: u16 = 3;

const ALPHA_NONE: u32 = 0;
const ALPHA_ASSOCIATED: u32 = 1;
const ALPHA_UNASSOCIATED: u32 = 2;

const MAX_ENTRIES: usize = 1024;

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: [u8; 4],
}

struct Chunk {
    remaining: u32,
    buf: [u8; 2048],
    pos: usize,
    len: usize,
}

unsafe impl Zeroable for Chunk {}

struct ChunkSource<'r, 'a> {
    r: &'r mut Reader<'a>,
    chunk: &'r mut Chunk,
}

impl Source for ChunkSource<'_, '_> {
    fn fill_buf(&mut self) -> compression::Result<&[u8]> {
        let s = &mut *self.chunk;
        if s.pos < s.len {
            return Ok(&s.buf[s.pos..s.len]);
        }
        s.pos = 0;
        let want = (s.remaining as usize).min(s.buf.len());
        let got = if want != 0 { self.r.read(&mut s.buf[..want]) } else { 0 };
        s.remaining -= got as u32;
        if got < want {
            s.remaining = 0;
        }
        s.len = got;
        Ok(&s.buf[..got])
    }

    fn consume(&mut self, n: usize) {
        self.chunk.pos += n;
    }
}

const fn type_size(kind: u16) -> usize {
    match kind {
        1 => 1,
        3 => 2,
        4 => 4,
        _ => 0,
    }
}

fn get16(big: bool, p: &[u8]) -> u32 {
    let v = [p[0], p[1]];
    u32::from(if big { u16::from_be_bytes(v) } else { u16::from_le_bytes(v) })
}

fn get32(big: bool, p: &[u8]) -> u32 {
    let v = [p[0], p[1], p[2], p[3]];
    if big { u32::from_be_bytes(v) } else { u32::from_le_bytes(v) }
}

fn read_values(r: &mut Reader, big: bool, e: &Entry, out: &mut [u32]) -> bool {
    let size = type_size(e.kind);
    if size == 0 || (e.count as usize) < out.len() {
        return false;
    }
    let inline = u64::from(e.count) * size as u64 <= 4;
    if !inline && !r.seek(get32(big, &e.value) as usize) {
        return false;
    }
    for (i, v) in out.iter_mut().enumerate() {
        let mut tmp = [0; 4];
        if inline {
            tmp[..size].copy_from_slice(&e.value[i * size..(i + 1) * size]);
        } else if !r.read_exact(&mut tmp[..size]) {
            return false;
        }
        *v = match size {
            1 => u32::from(tmp[0]),
            2 => get16(big, &tmp),
            _ => get32(big, &tmp),
        };
    }
    true
}

fn find(entries: &[Entry], tag: u16) -> Option<&Entry> {
    entries.iter().find(|e| e.tag == tag)
}

fn tag_value(r: &mut Reader, big: bool, entries: &[Entry], tag: u16, fallback: u32) -> u32 {
    let mut v = [0];
    match find(entries, tag) {
        Some(e) if read_values(r, big, e, &mut v) => v[0],
        _ => fallback,
    }
}

fn read_array(r: &mut Reader, big: bool, e: Option<&Entry>, n: u32, caps: Option<Caps>) -> Option<Buffer<u32>> {
    let e = e?;
    if e.count < n {
        return None;
    }
    let mut out: Buffer<u32> = buffer(n as usize, caps).ok()?;
    read_values(r, big, e, &mut out).then_some(out)
}

/// TIFF baseline, one image: strips or tiles, uncompressed, LZW, Deflate or
/// PackBits, up to 16 bits per sample, gray, RGB and palette, with alpha
/// composited over white. The directory and the strips may be anywhere in the
/// file, so this takes a stream that can seek.
pub struct TiffDecoder<'a> {
    r: Reader<'a>,
    big: bool,

    width: u16,
    height: u16,
    compression: u16,
    photometric: u16,
    predictor: u16,
    spp: usize,
    bps: usize,
    alpha: u32,
    palette: Box<[[u16; 256]; 3], Caps>,

    tiled: bool,
    chunk_w: usize,
    chunk_h: usize,
    across: usize,
    chunk_count: usize,
    offsets: Buffer<u32>,
    counts: Buffer<u32>,
    raw_bytes: usize,
    out_bpp: usize,

    chunk: Box<Chunk, Caps>,
    inflate: Option<(Box<Inflate, Caps>, Box<Window, Caps>)>,
    lzw: Option<(Box<Lzw, Caps>, Box<LzwWork, Caps>)>,
    pack_run: i32,
    pack_value: u8,

    raw: Buffer,
    band: Option<Buffer>,
    row: usize,
    next_chunk: usize,
    rows_left: usize,
    band_first: usize,
    band_rows: usize,
}

impl<'a> TiffDecoder<'a> {
    pub fn open(src: impl Stream + 'a, options: &DecodeOptions) -> Result<Self> {
        if !src.can_seek() {
            return Err(Error::Unsupported);
        }
        let src: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
        let mut r = Reader::new(src)?;
        let caps = options.caps;

        let mut hdr = [0; 8];
        if !r.read_exact(&mut hdr) {
            return Err(Error::Truncated);
        }
        let big = match &hdr[..4] {
            b"II*\0" => false,
            b"MM\0*" => true,
            _ => return Err(Error::Decode),
        };
        let mut n = [0; 2];
        if !r.seek(get32(big, &hdr[4..]) as usize) || !r.read_exact(&mut n) {
            return Err(Error::Truncated);
        }
        let count = get16(big, &n) as usize;
        if count == 0 || count > MAX_ENTRIES {
            return Err(Error::Decode);
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(|_| Error::OutOfMemory)?;
        for _ in 0..count {
            let mut raw = [0; 12];
            if !r.read_exact(&mut raw) {
                return Err(Error::Truncated);
            }
            entries.push(Entry {
                tag: get16(big, &raw) as u16,
                kind: get16(big, &raw[2..]) as u16,
                count: get32(big, &raw[4..]),
                value: [raw[8], raw[9], raw[10], raw[11]],
            });
        }
        let entries = &entries[..];
        let tag = |r: &mut Reader, tag: u16, fallback: u32| tag_value(r, big, entries, tag, fallback);

        let w = tag(&mut r, 256, 0);
        let h = tag(&mut r, 257, 0);
        let compression = tag(&mut r, 259, u32::from(COMP_NONE)) as u16;
        let photometric = tag(&mut r, 262, u32::from(PHOTO_BLACK_IS_ZERO)) as u16;
        let spp = tag(&mut r, 277, 1) as u16;
        let bps = tag(&mut r, 258, 1) as u16;
        let predictor = tag(&mut r, 317, 1) as u16;
        if w == 0 || h == 0 || spp == 0 {
            return Err(Error::Decode);
        }
        if w > 0xFFFF || h > 0xFFFF {
            return Err(Error::TooLarge);
        }
        if let Some(e) = find(entries, 258).filter(|e| e.count > 1) {
            let mut values = [0; 8];
            let k = (e.count as usize).min(8);
            if !read_values(&mut r, big, e, &mut values[..k]) || values[1..k].iter().any(|&v| v != values[0]) {
                return Err(Error::Unsupported);
            }
        }
        if (tag(&mut r, 284, 1) != 1 && spp > 1) || tag(&mut r, 339, 1) != 1 || tag(&mut r, 266, 1) != 1 {
            return Err(Error::Unsupported);
        }
        if !matches!(compression, COMP_NONE | COMP_LZW | COMP_DEFLATE | COMP_DEFLATE_OLD | COMP_PACKBITS) {
            return Err(Error::Unsupported);
        }
        if !matches!(bps, 1 | 2 | 4 | 8 | 16) || (predictor != 1 && (predictor != 2 || bps < 8)) {
            return Err(Error::Unsupported);
        }

        let mut palette: Box<[[u16; 256]; 3], Caps> = state()?;
        let mut colors = 1;
        match photometric {
            PHOTO_WHITE_IS_ZERO | PHOTO_BLACK_IS_ZERO => {}
            PHOTO_RGB => {
                if bps < 8 {
                    return Err(Error::Unsupported);
                }
                colors = 3;
            }
            PHOTO_PALETTE => {
                if bps > 8 || spp != 1 {
                    return Err(Error::Unsupported);
                }
                let n = 1usize << bps;
                let map = read_array(&mut r, big, find(entries, 320), n as u32 * 3, caps).ok_or(Error::Decode)?;
                for c in 0..3 {
                    for i in 0..n {
                        palette[c][i] = map[c * n + i] as u16;
                    }
                }
            }
            _ => return Err(Error::Unsupported),
        }
        if spp < colors {
            return Err(Error::Decode);
        }
        let extra = if spp > colors { tag(&mut r, 338, ALPHA_NONE) } else { 0 };
        let alpha = if extra == ALPHA_ASSOCIATED || extra == ALPHA_UNASSOCIATED { extra } else { ALPHA_NONE };

        let tiled = find(entries, 322).is_some();
        let (chunk_w, chunk_h, across, chunk_count, offsets_tag, counts_tag);
        if tiled {
            chunk_w = tag(&mut r, 322, 0);
            chunk_h = tag(&mut r, 323, 0);
            if chunk_w == 0 || chunk_h == 0 {
                return Err(Error::Decode);
            }
            across = w.wrapping_add(chunk_w - 1) / chunk_w;
            chunk_count = across.wrapping_mul(h.wrapping_add(chunk_h - 1) / chunk_h);
            (offsets_tag, counts_tag) = (324, 325);
        } else {
            let rps = tag(&mut r, 278, h);
            chunk_w = w;
            chunk_h = if rps == 0 || rps > h { h } else { rps };
            across = 1;
            chunk_count = h.div_ceil(chunk_h);
            (offsets_tag, counts_tag) = (273, 279);
        }

        let row_bits = u64::from(chunk_w) * u64::from(spp) * u64::from(bps);
        let raw_bytes = usize::try_from(row_bits.div_ceil(8)).map_err(|_| Error::Decode)?;
        let offsets = read_array(&mut r, big, find(entries, offsets_tag), chunk_count, caps).ok_or(Error::Decode)?;
        let counts = match find(entries, counts_tag) {
            Some(e) => read_array(&mut r, big, Some(e), chunk_count, caps),
            None if compression == COMP_NONE && chunk_count == 1 => {
                let mut one: Option<Buffer<u32>> = buffer(1, caps).ok();
                if let Some(one) = &mut one {
                    one[0] = (raw_bytes as u64 * u64::from(h)) as u32;
                }
                one
            }
            None => None,
        };
        let counts = counts.ok_or(Error::Decode)?;

        let out_bpp = if colors == 1 && photometric != PHOTO_PALETTE { 1 } else { 3 };
        let raw = buffer(raw_bytes, caps)?;
        let lzw = if compression == COMP_LZW { Some((state()?, heap::zeroed_box(default_caps())?)) } else { None };
        let inflate = if compression == COMP_DEFLATE || compression == COMP_DEFLATE_OLD {
            Some((state()?, heap::zeroed_box(default_caps())?))
        } else {
            None
        };
        let band = if tiled {
            let bytes = (chunk_h as usize).checked_mul(w as usize * out_bpp).ok_or(Error::OutOfMemory)?;
            Some(buffer(bytes, caps)?)
        } else {
            None
        };

        Ok(Self {
            r,
            big,
            width: w as u16,
            height: h as u16,
            compression,
            photometric,
            predictor,
            spp: usize::from(spp),
            bps: usize::from(bps),
            alpha,
            palette,
            tiled,
            chunk_w: chunk_w as usize,
            chunk_h: chunk_h as usize,
            across: across as usize,
            chunk_count: chunk_count as usize,
            offsets,
            counts,
            raw_bytes,
            out_bpp,
            chunk: state()?,
            inflate,
            lzw,
            pack_run: 0,
            pack_value: 0,
            raw,
            band,
            row: 0,
            next_chunk: 0,
            rows_left: 0,
            band_first: 0,
            band_rows: 0,
        })
    }

    pub fn info(&self) -> ImageInfo {
        let format = if self.out_bpp == 3 { PixelFormat::Rgb888 } else { PixelFormat::Gray8 };
        ImageInfo { width: self.width, height: self.height, format }
    }

    fn start_chunk(&mut self, index: usize) -> bool {
        if index >= self.chunk_count || !self.r.seek(self.offsets[index] as usize) {
            return false;
        }
        self.chunk.remaining = self.counts[index];
        self.chunk.pos = 0;
        self.chunk.len = 0;
        if let Some((lzw, work)) = &mut self.lzw {
            let options = LzwOptions { literal_bits: 8, msb_first: true, early_change: true };
            lzw.start(&options, work).is_ok()
        } else if let Some((inflate, _)) = &mut self.inflate {
            inflate.start(&mut ChunkSource { r: &mut self.r, chunk: &mut self.chunk }).is_ok()
        } else {
            self.pack_run = 0;
            true
        }
    }

    fn chunk_byte(&mut self) -> Option<u8> {
        if self.chunk.remaining == 0 {
            return None;
        }
        let c = self.r.byte()?;
        self.chunk.remaining -= 1;
        Some(c)
    }

    fn unpack_bits(&mut self, n: usize) -> bool {
        let mut i = 0;
        while i < n {
            if self.pack_run > 0 {
                let Some(c) = self.chunk_byte() else { return false };
                self.raw[i] = c;
                i += 1;
                self.pack_run -= 1;
            } else if self.pack_run < 0 {
                self.raw[i] = self.pack_value;
                i += 1;
                self.pack_run += 1;
            } else {
                let Some(c) = self.chunk_byte() else { return false };
                let c = i32::from(c);
                if c < 128 {
                    self.pack_run = c + 1;
                } else if c > 128 {
                    let Some(v) = self.chunk_byte() else { return false };
                    self.pack_value = v;
                    self.pack_run = -(257 - c);
                }
            }
        }
        true
    }

    fn read_raw_row(&mut self) -> bool {
        let n = self.raw_bytes;
        let mut src = ChunkSource { r: &mut self.r, chunk: &mut self.chunk };
        let ok = if let Some((lzw, work)) = &mut self.lzw {
            matches!(lzw.read(&mut src, work, &mut self.raw[..n]), Ok(got) if got == n)
        } else if let Some((inflate, window)) = &mut self.inflate {
            matches!(inflate.read(&mut src, window, &mut self.raw[..n]), Ok(got) if got == n)
        } else if self.compression == COMP_PACKBITS {
            self.unpack_bits(n)
        } else if (self.chunk.remaining as usize) < n || !self.r.read_exact(&mut self.raw[..n]) {
            false
        } else {
            self.chunk.remaining -= n as u32;
            true
        };
        if !ok {
            return false;
        }

        if self.predictor == 2 {
            let samples = self.chunk_w * self.spp;
            if self.bps == 8 {
                for i in self.spp..samples {
                    self.raw[i] = self.raw[i].wrapping_add(self.raw[i - self.spp]);
                }
            } else {
                let step = self.spp * 2;
                let hi = usize::from(!self.big);
                for i in (step..samples * 2).step_by(2) {
                    let word = |p: usize| (u32::from(self.raw[p + hi]) << 8) | u32::from(self.raw[p + 1 - hi]);
                    let v = word(i) + word(i - step);
                    self.raw[i + hi] = (v >> 8) as u8;
                    self.raw[i + 1 - hi] = v as u8;
                }
            }
        }
        true
    }

    #[inline]
    fn sample(&self, i: usize) -> u32 {
        match self.bps {
            8 => u32::from(self.raw[i]),
            16 => u32::from(self.raw[2 * i + usize::from(!self.big)]),
            bps => {
                let bit = i * bps;
                u32::from(self.raw[bit >> 3] >> (8 - bps - (bit & 7))) & ((1 << bps) - 1)
            }
        }
    }

    #[inline]
    fn scaled(&self, v: u32) -> u8 {
        if self.bps >= 8 { v as u8 } else { (v * 255 / ((1 << self.bps) - 1)) as u8 }
    }

    fn over_white(&self, c: u8, a: u8) -> u8 {
        let (c, a) = (u32::from(c), u32::from(a));
        if self.alpha == ALPHA_ASSOCIATED {
            (c + 255 - a).min(255) as u8
        } else {
            ((c * a + 255 * (255 - a) + 127) / 255) as u8
        }
    }

    fn convert(&self, dst: &mut [u8], pixels: usize) {
        for x in 0..pixels {
            let s = x * self.spp;
            if self.photometric == PHOTO_PALETTE {
                let idx = self.sample(s) as usize;
                for c in 0..3 {
                    dst[3 * x + c] = (self.palette[c][idx] >> 8) as u8;
                }
            } else if self.photometric == PHOTO_RGB {
                for c in 0..3 {
                    let v = self.scaled(self.sample(s + c));
                    dst[3 * x + c] =
                        if self.alpha != ALPHA_NONE { self.over_white(v, self.scaled(self.sample(s + 3))) } else { v };
                }
            } else {
                let mut v = self.scaled(self.sample(s));
                if self.photometric == PHOTO_WHITE_IS_ZERO {
                    v = 255 - v;
                }
                dst[x] = if self.alpha != ALPHA_NONE { self.over_white(v, self.scaled(self.sample(s + 1))) } else { v };
            }
        }
    }

    fn load_band(&mut self) -> Result<()> {
        let w = usize::from(self.width);
        let tile_row = self.row / self.chunk_h;
        self.band_first = tile_row * self.chunk_h;
        self.band_rows = (usize::from(self.height) - self.band_first).min(self.chunk_h);
        let stride = w * self.out_bpp;
        let mut band = self.band.take().unwrap();
        let mut result = Ok(());
        'tiles: for tx in 0..self.across {
            if !self.start_chunk(tile_row * self.across + tx) {
                result = Err(Error::Decode);
                break;
            }
            let x0 = tx * self.chunk_w;
            let pixels = (w - x0).min(self.chunk_w);
            for r in 0..self.band_rows {
                if !self.read_raw_row() {
                    result = Err(Error::Truncated);
                    break 'tiles;
                }
                self.convert(&mut band[r * stride + x0 * self.out_bpp..], pixels);
            }
        }
        self.band = Some(band);
        result
    }

    /// Writes a row and returns `true`; `false` at the end of the image.
    pub fn next_row(&mut self, dst: &mut [u8]) -> Result<bool> {
        if dst.len() < self.info().row_bytes() {
            return Err(Error::InvalidArgument);
        }
        if self.row >= usize::from(self.height) {
            return Ok(false);
        }
        if self.tiled {
            if self.row >= self.band_first + self.band_rows || self.band_rows == 0 {
                self.load_band()?;
            }
            let stride = usize::from(self.width) * self.out_bpp;
            let at = (self.row - self.band_first) * stride;
            dst[..stride].copy_from_slice(&self.band.as_ref().unwrap()[at..at + stride]);
            self.row += 1;
            return Ok(true);
        }
        if self.rows_left == 0 {
            let index = self.next_chunk;
            self.next_chunk += 1;
            if !self.start_chunk(index) {
                return Err(Error::Decode);
            }
            self.rows_left = self.chunk_h;
        }
        if !self.read_raw_row() {
            return Err(Error::Truncated);
        }
        self.convert(dst, usize::from(self.width));
        self.rows_left -= 1;
        self.row += 1;
        Ok(true)
    }
}
