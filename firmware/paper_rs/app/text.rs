/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Text on an L4 canvas. The renderer has no gray target, so each run is
//! drawn white on black at 16 bits and taken as its coverage.

use alloc::vec;
use alloc::vec::Vec;

use devkit_gfx::{ClipStore, Color, DisplayList, Font, FontChain, GlyphCache, Prepared, Rect, Rgb565, Surface, TextOptions, TextRun};

use crate::resources;

const GLYPH_CACHE: usize = 16 * 1024;

static FONTS: [&Font; 1] = [&resources::BODY_24];

/// `w` x `h` pixels in L4. Grays are given and read as 8 bits.
pub(crate) struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

fn level(gray: u8) -> u8 {
    ((u16::from(gray) * 15 + 127) / 255) as u8
}

impl Canvas {
    pub(crate) fn new(w: usize, h: usize, gray: u8) -> Self {
        Self { w, h, px: vec![level(gray) * 0x11; w.div_ceil(2) * h] }
    }

    fn at(&self, x: usize, y: usize) -> (usize, u32) {
        (y * self.w.div_ceil(2) + x / 2, if x % 2 == 0 { 4 } else { 0 })
    }

    fn get(&self, x: usize, y: usize) -> u8 {
        let (i, shift) = self.at(x, y);
        (self.px[i] >> shift & 0x0F) * 0x11
    }

    fn set(&mut self, x: usize, y: usize, gray: u8) {
        let (i, shift) = self.at(x, y);
        self.px[i] = self.px[i] & !(0x0F << shift) | level(gray) << shift;
    }

    pub(crate) fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, gray: u8) {
        for row in y..(y + h).min(self.h) {
            for col in x..(x + w).min(self.w) {
                self.set(col, row, gray);
            }
        }
    }
}

pub(crate) struct Writer {
    chain: FontChain,
    cache: GlyphCache,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self { chain: FontChain::new(&FONTS), cache: GlyphCache::new(GLYPH_CACHE) }
    }

    /// Size of `text` as it would be drawn.
    pub(crate) fn measure(&self, text: &str) -> (usize, usize) {
        let mut run = TextRun::new();
        run.layout(&self.chain, text, &TextOptions::default());
        (run.width() as usize, run.height() as usize)
    }

    /// Draws `text` with its top left at `x`, `y`, clipped to the canvas.
    pub(crate) fn draw(&mut self, canvas: &mut Canvas, x: usize, y: usize, text: &str, ink: u8) {
        let mut run = TextRun::new();
        run.layout(&self.chain, text, &TextOptions::default());
        let rw = (run.width() as usize).min(canvas.w.saturating_sub(x));
        let rh = (run.height() as usize).min(canvas.h.saturating_sub(y));
        if rw == 0 || rh == 0 {
            return;
        }
        let area = Rect::new(0, 0, rw as i16, rh as i16);
        let mut list = DisplayList::new();
        list.text(0, 0, &run, Color::rgb(0xFF, 0xFF, 0xFF));
        let mut prepared = Prepared::new();
        prepared.prepare(&list, &[area], &mut self.cache);
        let mut scratch = vec![0u16; rw * rh];
        let surface = unsafe { Surface::new(scratch.as_mut_ptr().cast(), rw * 2, area) };
        devkit_gfx::draw::<Rgb565, _, _, _>(&list, &prepared, &mut ClipStore::new(), &surface);
        for (row, src) in scratch.chunks_exact(rw).enumerate() {
            for (col, &s) in src.iter().enumerate() {
                let (cx, cy) = (x + col, y + row);
                let d = i32::from(canvas.get(cx, cy));
                let cover = i32::from((s >> 5) & 0x3F) * 255 / 63;
                canvas.set(cx, cy, (d + (i32::from(ink) - d) * cover / 255) as u8);
            }
        }
    }
}
