/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Text on an L8 canvas. The renderer has no 8-bit gray target, so each run
//! is drawn white on black at 16 bits and taken as its coverage.

use alloc::vec;
use alloc::vec::Vec;

use devkit_gfx::{ClipStore, Color, DisplayList, Font, FontChain, GlyphCache, Prepared, Rect, Rgb565, Surface, TextOptions, TextRun};

use crate::resources;

const GLYPH_CACHE: usize = 16 * 1024;

static FONTS: [&Font; 1] = [&resources::BODY_24];

/// `w` x `h` gray pixels.
pub(crate) struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Canvas {
    pub(crate) fn new(w: usize, h: usize, gray: u8) -> Self {
        Self { w, h, px: vec![gray; w * h] }
    }

    pub(crate) fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, gray: u8) {
        for row in y..(y + h).min(self.h) {
            self.px[row * self.w + x..row * self.w + (x + w).min(self.w)].fill(gray);
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
            let dst = &mut canvas.px[(y + row) * canvas.w + x..][..rw];
            for (d, &s) in dst.iter_mut().zip(src) {
                let cover = i32::from((s >> 5) & 0x3F) * 255 / 63;
                *d = (i32::from(*d) + (i32::from(ink) - i32::from(*d)) * cover / 255) as u8;
            }
        }
    }
}
