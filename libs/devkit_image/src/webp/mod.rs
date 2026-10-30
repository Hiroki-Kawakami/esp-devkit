/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod lossless;
mod lossy;

use alloc::boxed::Box;

use idf::heap::Caps;

use crate::mem::{Buffer, buffer};
use crate::{DecodeOptions, Error, ImageInfo, PixelFormat, Result, Stream};
use lossless::Vp8l;
use lossy::Vp8;

const ALPHA_WINDOW: usize = 8192;

pub(crate) struct Source<'a> {
    stream: Box<dyn Stream + 'a>,
    at: usize,
    truncated: bool,
}

impl Source<'_> {
    /// Reads at `pos`. Without seek only forward moves work.
    fn read(&mut self, pos: usize, dst: &mut [u8]) -> usize {
        if self.at != pos {
            if self.stream.can_seek() {
                if self.stream.seek(pos).is_err() {
                    self.truncated = true;
                    return 0;
                }
                self.at = pos;
            } else {
                let mut tmp = [0; 256];
                while self.at < pos {
                    let k = (pos - self.at).min(tmp.len());
                    match self.stream.read(&mut tmp[..k]) {
                        Ok(got) if got > 0 => self.at += got,
                        _ => {
                            self.truncated = true;
                            return 0;
                        }
                    }
                }
                if self.at != pos {
                    self.truncated = true;
                    return 0;
                }
            }
        }
        let mut total = 0;
        while total < dst.len() {
            match self.stream.read(&mut dst[total..]) {
                Ok(got) if got > 0 => total += got,
                _ => break,
            }
        }
        self.at += total;
        if total < dst.len() {
            self.truncated = true;
        }
        total
    }

    /// What a codec reports when its bits ran out: a file cut short, or a chunk
    /// shorter than its contents.
    fn eof_error(&self) -> Error {
        if self.truncated { Error::Truncated } else { Error::Decode }
    }
}

pub(crate) struct Cursor {
    buf: Buffer,
    next: usize,
    end: usize,
}

impl Cursor {
    fn new(start: usize, len: usize, cap: usize, caps: Option<Caps>) -> Result<Self> {
        let cap = if cap > len { len.max(1) } else { cap };
        Ok(Self { buf: buffer(cap, caps)?, next: start, end: start + len })
    }

    /// The next block of the range, in `buf`; 0 once the range or the stream ends.
    fn refill(&mut self, src: &mut Source) -> usize {
        let want = self.end.saturating_sub(self.next).min(self.buf.len());
        if want == 0 {
            return 0;
        }
        let got = src.read(self.next, &mut self.buf[..want]);
        self.next += got;
        if got < want {
            self.end = self.next;
        }
        got
    }
}

#[derive(Clone, Copy, Default)]
struct Chunk {
    pos: usize,
    len: usize,
}

impl Chunk {
    const fn end(&self) -> usize {
        self.pos + self.len + (self.len & 1)
    }
}

fn le24(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], 0])
}

fn le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

fn over_white(c: u8, a: u8) -> u8 {
    let (c, a) = (u32::from(c), u32::from(a));
    ((c * a + 255 * (255 - a) + 127) / 255) as u8
}

fn unfilter(filter: u8, prev: Option<&[u8]>, input: &[u8], out: &mut [u8]) {
    let Some(prev) = prev.filter(|_| filter != 0 && filter != 1) else {
        if filter == 0 {
            out.copy_from_slice(input);
        } else {
            let mut pred = prev.map_or(0, |p| p[0]);
            for (o, &i) in out.iter_mut().zip(input) {
                pred = pred.wrapping_add(i);
                *o = pred;
            }
        }
        return;
    };
    if filter == 2 {
        for ((o, &i), &p) in out.iter_mut().zip(input).zip(prev) {
            *o = p.wrapping_add(i);
        }
        return;
    }
    let (mut top_left, mut left) = (prev[0], prev[0]);
    for ((o, &i), &top) in out.iter_mut().zip(input).zip(prev) {
        let g = (i32::from(left) + i32::from(top) - i32::from(top_left)).clamp(0, 255);
        left = i.wrapping_add(g as u8);
        top_left = top;
        *o = left;
    }
}

fn read_chunk(src: &mut Source, pos: usize, end: usize) -> Result<([u8; 4], Chunk)> {
    let mut hdr = [0; 8];
    if pos + 8 > end {
        return Err(Error::Decode);
    }
    if src.read(pos, &mut hdr) != 8 {
        return Err(Error::Truncated);
    }
    let chunk = Chunk { pos: pos + 8, len: le32(&hdr[4..]) as usize };
    if chunk.len > end - chunk.pos {
        return Err(Error::Decode);
    }
    Ok(([hdr[0], hdr[1], hdr[2], hdr[3]], chunk))
}

struct Layout {
    alph: Chunk,
    image: Chunk,
    lossless: bool,
    canvas: Option<(u32, u32)>,
    frame: Option<(u32, u32, u32, u32)>,
}

fn find_image(src: &mut Source, mut pos: usize, end: usize, layout: &mut Layout) -> Result<()> {
    loop {
        let (tag, c) = read_chunk(src, pos, end)?;
        if &tag == b"VP8 " || &tag == b"VP8L" {
            layout.image = c;
            layout.lossless = tag[3] == b'L';
            return Ok(());
        }
        if &tag == b"ALPH" {
            layout.alph = c;
        }
        pos = c.end();
    }
}

fn parse(src: &mut Source) -> Result<Layout> {
    let mut riff = [0; 12];
    if src.read(0, &mut riff) != 12 {
        return Err(Error::Truncated);
    }
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WEBP" {
        return Err(Error::Decode);
    }
    let end = 8 + le32(&riff[4..]) as usize;
    let mut layout = Layout { alph: Chunk::default(), image: Chunk::default(), lossless: false, canvas: None, frame: None };

    let (tag, first) = read_chunk(src, 12, end)?;
    if &tag != b"VP8X" {
        layout.image = first;
        layout.lossless = tag[3] == b'L';
        return if &tag == b"VP8 " || &tag == b"VP8L" { Ok(layout) } else { Err(Error::Decode) };
    }

    let mut vp8x = [0; 10];
    if first.len < 10 {
        return Err(Error::Decode);
    }
    if src.read(first.pos, &mut vp8x) != 10 {
        return Err(Error::Truncated);
    }
    layout.canvas = Some((le24(&vp8x[4..]) + 1, le24(&vp8x[7..]) + 1));
    if vp8x[0] & 0x02 == 0 {
        find_image(src, first.end(), end, &mut layout)?;
        return Ok(layout);
    }

    let mut pos = first.end();
    let frame = loop {
        let (tag, frame) = read_chunk(src, pos, end)?;
        if &tag == b"ANMF" {
            break frame;
        }
        pos = frame.end();
    };
    let mut anmf = [0; 16];
    if frame.len < 16 {
        return Err(Error::Decode);
    }
    if src.read(frame.pos, &mut anmf) != 16 {
        return Err(Error::Truncated);
    }
    layout.frame = Some((le24(&anmf) * 2, le24(&anmf[3..]) * 2, le24(&anmf[6..]) + 1, le24(&anmf[9..]) + 1));
    find_image(src, frame.pos + 16, frame.pos + frame.len, &mut layout)?;
    Ok(layout)
}

enum AlphaInput {
    Raw { cursor: Cursor, pos: usize, len: usize },
    Lossless { vp8l: Box<Vp8l>, argb: Buffer<u32> },
}

struct Alpha {
    filter: u8,
    input: AlphaInput,
    row_in: Buffer,
    row: Buffer,
    prev: Buffer,
}

impl Alpha {
    fn open(src: &mut Source, alph: Chunk, w: u32, h: u32, caps: Option<Caps>) -> Result<Self> {
        let mut hdr = [0];
        if alph.len < 2 {
            return Err(Error::Decode);
        }
        if src.read(alph.pos, &mut hdr) != 1 {
            return Err(Error::Truncated);
        }
        let hdr = hdr[0];
        let (method, pre, filter) = (hdr & 3, (hdr >> 4) & 3, (hdr >> 2) & 3);
        if method > 1 || pre > 1 || hdr >> 6 != 0 {
            return Err(Error::Decode);
        }
        let width = w as usize;
        let input = if method == 0 {
            if alph.len - 1 < width * h as usize {
                return Err(Error::Decode);
            }
            AlphaInput::Raw { cursor: Cursor::new(alph.pos + 1, alph.len - 1, ALPHA_WINDOW, caps)?, pos: 0, len: 0 }
        } else {
            let vp8l = Vp8l::open(src, alph.pos + 1, alph.len - 1, w, h, caps)?;
            AlphaInput::Lossless { vp8l: Box::try_new(vp8l).map_err(|_| Error::OutOfMemory)?, argb: buffer(width, caps)? }
        };
        Ok(Self { filter, input, row_in: buffer(width, caps)?, row: buffer(width, caps)?, prev: buffer(width, caps)? })
    }

    fn next_row(&mut self, src: &mut Source, y: u32) -> Result<()> {
        match &mut self.input {
            AlphaInput::Lossless { vp8l, argb } => {
                vp8l.next_row(src, argb)?;
                for (i, &p) in self.row_in.iter_mut().zip(argb.iter()) {
                    *i = (p >> 8) as u8;
                }
            }
            AlphaInput::Raw { cursor, pos, len } => {
                let mut done = 0;
                while done < self.row_in.len() {
                    if *pos == *len {
                        *len = cursor.refill(src);
                        *pos = 0;
                        if *len == 0 {
                            return Err(src.eof_error());
                        }
                    }
                    let k = (*len - *pos).min(self.row_in.len() - done);
                    self.row_in[done..done + k].copy_from_slice(&cursor.buf[*pos..*pos + k]);
                    *pos += k;
                    done += k;
                }
            }
        }
        core::mem::swap(&mut self.row, &mut self.prev);
        unfilter(self.filter, (y != 0).then_some(&self.prev[..]), &self.row_in, &mut self.row);
        Ok(())
    }
}

enum Codec {
    Lossy(Box<Vp8>),
    Lossless { vp8l: Box<Vp8l>, argb: Buffer<u32> },
}

/// WebP: lossy, lossless, ALPH planes and the first frame of an animation, with
/// alpha composited over white. Only a lossless picture does without a stream
/// that can seek. The output matches libwebp byte for byte.
pub struct WebpDecoder<'a> {
    src: Source<'a>,
    codec: Codec,
    has_alpha: bool,
    alpha: Option<Alpha>,
    width: u16,
    height: u16,
    frame_x: u32,
    frame_y: u32,
    frame_w: u32,
    frame_h: u32,
    row: u32,
}

impl<'a> WebpDecoder<'a> {
    pub fn open(src: impl Stream + 'a, options: &DecodeOptions) -> Result<Self> {
        let stream: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
        let can_seek = stream.can_seek();
        let mut src = Source { stream, at: 0, truncated: false };
        let caps = options.caps;

        let mut layout = parse(&mut src)?;
        if layout.lossless {
            layout.alph.len = 0;
        }
        if (!layout.lossless || layout.alph.len != 0) && !can_seek {
            return Err(Error::Unsupported);
        }

        let (w, h, has_alpha, codec);
        if layout.lossless {
            let vp8l = Vp8l::open(&mut src, layout.image.pos, layout.image.len, 0, 0, caps)?;
            (w, h) = (vp8l.width(), vp8l.height());
            has_alpha = vp8l.has_alpha();
            let vp8l = Box::try_new(vp8l).map_err(|_| Error::OutOfMemory)?;
            codec = Codec::Lossless { vp8l, argb: buffer(w as usize, caps)? };
        } else {
            let vp8 = Vp8::open(&mut src, layout.image.pos, layout.image.len, caps)?;
            (w, h) = (vp8.width(), vp8.height());
            has_alpha = layout.alph.len != 0;
            codec = Codec::Lossy(Box::try_new(vp8).map_err(|_| Error::OutOfMemory)?);
        }

        let (frame_x, frame_y, frame_w, frame_h, canvas_w, canvas_h);
        if let Some((x, y, fw, fh)) = layout.frame {
            let (cw, ch) = layout.canvas.unwrap_or((0, 0));
            if fw != w || fh != h || x + fw > cw || y + fh > ch {
                return Err(Error::Decode);
            }
            (frame_x, frame_y, frame_w, frame_h, canvas_w, canvas_h) = (x, y, fw, fh, cw, ch);
        } else {
            if layout.canvas.is_some_and(|c| c != (w, h)) {
                return Err(Error::Decode);
            }
            (frame_x, frame_y, frame_w, frame_h, canvas_w, canvas_h) = (0, 0, w, h, w, h);
        }
        if canvas_w > 0xFFFF || canvas_h > 0xFFFF {
            return Err(Error::TooLarge);
        }

        let alpha = if has_alpha && !layout.lossless {
            Some(Alpha::open(&mut src, layout.alph, frame_w, frame_h, caps)?)
        } else {
            None
        };

        Ok(Self {
            src,
            codec,
            has_alpha,
            alpha,
            width: canvas_w as u16,
            height: canvas_h as u16,
            frame_x,
            frame_y,
            frame_w,
            frame_h,
            row: 0,
        })
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo { width: self.width, height: self.height, format: PixelFormat::Rgb888 }
    }

    fn frame_row(&mut self, out: &mut [u8], y: u32) -> Result<()> {
        let src = &mut self.src;
        match &mut self.codec {
            Codec::Lossless { vp8l, argb } => {
                vp8l.next_row(src, argb)?;
                for (o, &p) in out.chunks_exact_mut(3).zip(argb.iter()) {
                    let (a, r, g, b) = ((p >> 24) as u8, (p >> 16) as u8, (p >> 8) as u8, p as u8);
                    if self.has_alpha {
                        o[0] = over_white(r, a);
                        o[1] = over_white(g, a);
                        o[2] = over_white(b, a);
                    } else {
                        o[0] = r;
                        o[1] = g;
                        o[2] = b;
                    }
                }
            }
            Codec::Lossy(vp8) => {
                vp8.next_row(src, out)?;
                if let Some(alpha) = &mut self.alpha {
                    alpha.next_row(src, y)?;
                    for (o, &a) in out.chunks_exact_mut(3).zip(alpha.row.iter()) {
                        o[0] = over_white(o[0], a);
                        o[1] = over_white(o[1], a);
                        o[2] = over_white(o[2], a);
                    }
                }
            }
        }
        Ok(())
    }

    /// Writes a row and returns `true`; `false` at the end of the image.
    pub fn next_row(&mut self, dst: &mut [u8]) -> Result<bool> {
        let row_bytes = self.info().row_bytes();
        if dst.len() < row_bytes {
            return Err(Error::InvalidArgument);
        }
        if self.row >= u32::from(self.height) {
            return Ok(false);
        }
        let y = self.row;
        let inside = y >= self.frame_y && y < self.frame_y + self.frame_h;
        if !inside || self.frame_w != u32::from(self.width) {
            dst[..row_bytes].fill(0xFF);
        }
        if inside {
            let at = self.frame_x as usize * 3;
            self.frame_row(&mut dst[at..at + self.frame_w as usize * 3], y - self.frame_y)?;
        }
        self.row += 1;
        Ok(true)
    }
}
