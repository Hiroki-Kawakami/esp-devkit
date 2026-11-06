/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_gfx::simd::blend565;
use devkit_gfx::{Argb8888, Bgr888, Color, ImageFormat, Pixel, Raster, RasterTarget, Rect, Rgb565};
use idf::heap::Caps;

const BACK_SCALE: f32 = 0.9;
const FRONT_SCALE: f32 = 1.1;
const OUTSIDE: u16 = u16::MAX;
/// Size of both pictures against the pane: half, as `Surface::half` draws
/// them, and in the 2D engine's steps of 1/16.
pub(crate) const PICTURE_SCALE: f32 = 0.5;
pub(crate) const CHUNK: usize = 128;

#[repr(C, align(16))]
pub(crate) struct Line(pub(crate) [u16; CHUNK]);

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Block([u16; 32]);

/// An RGB565 picture of a pane at `PICTURE_SCALE`, cache-line aligned so the
/// 2D engine can write it.
pub(crate) struct Picture {
    blocks: Vec<Block, Caps>,
    pub(crate) width: i16,
    pub(crate) height: i16,
}

/// What the 2D engine makes of `length` pixels at `PICTURE_SCALE`.
pub(crate) fn scaled(length: i16) -> i16 {
    let sixteenths = (PICTURE_SCALE * 16.0) as i32;
    (i32::from(length) * sixteenths / 16) as i16
}

impl Picture {
    pub(crate) fn allocate(area: Rect) -> Option<Self> {
        let (width, height) = (scaled(area.w), scaled(area.h));
        let count = (width as usize * height as usize).div_ceil(32);
        let mut blocks = Vec::new_in(Caps::SPIRAM);
        blocks.try_reserve_exact(count).ok()?;
        blocks.resize(count, Block([0; 32]));
        Some(Self { blocks, width, height })
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u16 {
        self.blocks.as_mut_ptr().cast()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.blocks.len() * size_of::<Block>()
    }

    fn row(&self, y: u16) -> *const u16 {
        unsafe { self.blocks.as_ptr().cast::<u16>().add(usize::from(y) * self.width as usize) }
    }
}


/// Where each pane row and column reads from a picture: `OUTSIDE` past its edge.
struct Layer {
    rows: Vec<u16, Caps>,
    columns: Vec<u16, Caps>,
    /// The columns inside, which are contiguous.
    span: (usize, usize),
    /// `columns` as byte offsets into a row.
    offsets: Vec<u16, Caps>,
}

impl Layer {
    fn new(pane: Rect) -> Option<Self> {
        let mut rows = Vec::new_in(Caps::INTERNAL);
        let mut columns = Vec::new_in(Caps::INTERNAL);
        rows.try_reserve_exact(pane.h as usize).ok()?;
        columns.try_reserve_exact(pane.w as usize).ok()?;
        let mut offsets = Vec::new_in(Caps::INTERNAL);
        offsets.try_reserve_exact(pane.w as usize).ok()?;
        rows.resize(pane.h as usize, OUTSIDE);
        columns.resize(pane.w as usize, OUTSIDE);
        offsets.resize(pane.w as usize, 0);
        Some(Self { rows, columns, span: (0, 0), offsets })
    }

    fn map(&mut self, picture: &Picture, scale: f32) {
        let fill = |out: &mut [u16], limit: i16| {
            let middle = out.len() as f32 / 2.0;
            for (at, slot) in out.iter_mut().enumerate() {
                let source = ((at as f32 + 0.5 - middle) / scale + middle) * PICTURE_SCALE;
                *slot = if source >= 0.0 && (source as i16) < limit { source as u16 } else { OUTSIDE };
            }
        };
        fill(&mut self.rows, picture.height);
        fill(&mut self.columns, picture.width);
        let first = self.columns.iter().position(|&c| c != OUTSIDE).unwrap_or(0);
        let end = self.columns.iter().rposition(|&c| c != OUTSIDE).map_or(0, |last| last + 1);
        self.span = (first, end.max(first));
        for (offset, &column) in self.offsets.iter_mut().zip(&self.columns) {
            *offset = column.wrapping_mul(2);
        }
    }
}

pub(crate) struct Zoom {
    /// The pane on the screen.
    pub(crate) area: Rect,
    entering: Picture,
    leaving: Picture,
    background: u16,
    forward: bool,
    back: Layer,
    front: Layer,
    a5: u32,
}

impl Zoom {
    pub(crate) fn new(area: Rect, entering: Picture, leaving: Picture, background: Color, forward: bool) -> Option<Self> {
        let mut packed = 0u16;
        unsafe { Rgb565::fill((&raw mut packed).cast(), 1, Rgb565::pack(background)) };
        let (back, front) = (Layer::new(area)?, Layer::new(area)?);
        let mut zoom = Self { area, entering, leaving, background: packed, forward, back, front, a5: 32 };
        zoom.set_progress(0.0);
        Some(zoom)
    }

    pub(crate) fn set_progress(&mut self, progress: f32) {
        let q = if self.forward { progress } else { 1.0 - progress };
        let (back, front) = if self.forward { (&self.entering, &self.leaving) } else { (&self.leaving, &self.entering) };
        self.back.map(back, BACK_SCALE + (1.0 - BACK_SCALE) * q);
        self.front.map(front, 1.0 + (FRONT_SCALE - 1.0) * q);
        self.a5 = ((1.0 - q) * 32.0 + 0.5) as u32;
    }

    fn pictures(&self) -> (&Picture, &Picture) {
        if self.forward { (&self.entering, &self.leaving) } else { (&self.leaving, &self.entering) }
    }

    /// Column by column of `CHUNK` pixels: rows reading the same source row as
    /// the one above them reuse what it gathered.
    fn paint<P: Pixel>(&self, target: &mut RasterTarget<'_>) {
        let area = target.area();
        let (x, n) = (area.x as usize, area.w as usize);
        let (back, front) = self.pictures();
        let mut line = Line([0; CHUNK]);
        let mut backs = Line([0; CHUNK]);
        let mut fronts = Line([0; CHUNK]);
        let direct = P::NATIVE == ImageFormat::Rgb565;
        let mut done = 0;
        while done < n {
            let count = (n - done).min(CHUNK);
            let (from, to) = (x + done, x + done + count);
            let (back_lo, back_hi) = (self.back.span.0.clamp(from, to), self.back.span.1.clamp(from, to));
            let (front_lo, front_hi) = (self.front.span.0.clamp(from, to), self.front.span.1.clamp(from, to));
            let (mut last_back, mut last_front) = (None, None);
            for y in area.y..area.bottom() {
                let out = target.row(y).as_mut_ptr();
                let buffer: *mut u16 = if direct { unsafe { out.add(done * 2).cast() } } else { line.0.as_mut_ptr() };
                let back_row = self.back.rows[y as usize];
                let front_row = self.front.rows[y as usize];
                let back_row = (back_row != OUTSIDE && self.a5 < 32).then_some(back_row);
                let front_row = (front_row != OUTSIDE && self.a5 > 0).then_some(front_row);
                unsafe {
                    if last_back != Some(back_row) {
                        match back_row {
                            Some(row) => {
                                fill(backs.0.as_mut_ptr(), back_lo - from, self.background);
                                gather(backs.0.as_mut_ptr().add(back_lo - from), back.row(row), &self.back.offsets[back_lo..back_hi]);
                                fill(backs.0.as_mut_ptr().add(back_hi - from), to - back_hi, self.background);
                            }
                            None => fill(backs.0.as_mut_ptr(), count, self.background),
                        }
                        last_back = Some(back_row);
                    }
                    if last_front != Some(front_row) {
                        if let Some(row) = front_row {
                            gather(fronts.0.as_mut_ptr(), front.row(row), &self.front.offsets[front_lo..front_hi]);
                        }
                        last_front = Some(front_row);
                    }
                    let (lo, hi) = (front_lo - from, front_hi - from);
                    match front_row {
                        Some(_) if self.a5 >= 32 => {
                            core::ptr::copy_nonoverlapping(backs.0.as_ptr(), buffer, lo);
                            core::ptr::copy_nonoverlapping(fronts.0.as_ptr(), buffer.add(lo), hi - lo);
                            core::ptr::copy_nonoverlapping(backs.0.as_ptr().add(hi), buffer.add(hi), count - hi);
                        }
                        Some(_) => {
                            core::ptr::copy_nonoverlapping(backs.0.as_ptr(), buffer, lo);
                            blend565(buffer.add(lo), backs.0.as_ptr().add(lo), fronts.0.as_ptr(), hi - lo, self.a5);
                            core::ptr::copy_nonoverlapping(backs.0.as_ptr().add(hi), buffer.add(hi), count - hi);
                        }
                        None => core::ptr::copy_nonoverlapping(backs.0.as_ptr(), buffer, count),
                    }
                    if !direct {
                        store::<P>(out.add(done * P::BYTES), core::slice::from_raw_parts(buffer, count));
                    }
                }
            }
            done += count;
        }
    }
}

#[inline(always)]
unsafe fn fill(out: *mut u16, n: usize, value: u16) {
    for i in 0..n {
        unsafe { out.add(i).write(value) };
    }
}

/// The pixels at byte `offsets`, all inside the row `source`.
#[inline(always)]
pub(crate) unsafe fn gather(out: *mut u16, source: *const u16, offsets: &[u16]) {
    let (n, offsets, source) = (offsets.len(), offsets.as_ptr(), source.cast::<u8>());
    let at = |i: usize| unsafe { source.add(usize::from(offsets.add(i).read())).cast::<u16>().read() };
    let mut i = 0;
    while i + 4 <= n {
        let (a, b, c, d) = (at(i), at(i + 1), at(i + 2), at(i + 3));
        unsafe {
            out.add(i).write(a);
            out.add(i + 1).write(b);
            out.add(i + 2).write(c);
            out.add(i + 3).write(d);
        }
        i += 4;
    }
    while i < n {
        unsafe { out.add(i).write(at(i)) };
        i += 1;
    }
}


unsafe fn store<P: Pixel>(out: *mut u8, line: &[u16]) {
    for (i, &v) in line.iter().enumerate() {
        let (r, g, b) = ((v >> 11) as u8, (v >> 5) as u8 & 0x3f, v as u8 & 0x1f);
        let (r, g, b) = (r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2);
        let d = unsafe { out.add(i * P::BYTES) };
        unsafe {
            d.write(b);
            d.add(1).write(g);
            d.add(2).write(r);
            if P::BYTES == 4 {
                d.add(3).write(255);
            }
        }
    }
}

impl Raster for Zoom {
    fn raster(&self, target: &mut RasterTarget<'_>) {
        match target.format() {
            ImageFormat::Bgr888 => self.paint::<Bgr888>(target),
            ImageFormat::Argb8888 => self.paint::<Argb8888>(target),
            _ => self.paint::<Rgb565>(target),
        }
    }
}
