/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::format::{FONT_DATA_SLACK, FONT_PREFILTER, FontHeader, GLYPH_COMPRESSED, GLYPH_OFFSET_MASK, GLYPH_SIZE, GlyphRecord, LevelReader};

pub struct Font {
    data: &'static [u8],
    header: FontHeader,
}

impl Font {
    /// Reads a resource binary; panics, at compile time for a `static`, when it
    /// is not one.
    pub const fn parse(data: &'static [u8]) -> Self {
        let Some(header) = FontHeader::read(data) else { panic!("not a font resource") };
        Self { data, header }
    }

    pub const fn px(&self) -> u16 {
        self.header.px
    }

    pub const fn line_height(&self) -> i16 {
        self.header.line_height
    }

    /// The baseline's distance from the bottom of the line.
    pub const fn base_line(&self) -> i16 {
        self.header.base_line
    }

    /// The tallest included glyph above the baseline.
    pub const fn max_ascent(&self) -> i16 {
        self.header.max_ascent
    }

    /// The deepest included glyph below the baseline.
    pub const fn max_descent(&self) -> i16 {
        self.header.max_descent
    }

    pub(crate) fn find(&self, codepoint: u32) -> Option<u32> {
        let table = &self.data[self.header.codepoints as usize..];
        let at = |i: usize| {
            let p = &table[i * 4..i * 4 + 4];
            u32::from_le_bytes([p[0], p[1], p[2], p[3]])
        };
        let (mut low, mut high) = (0, self.header.glyph_count as usize);
        while low < high {
            let middle = (low + high) / 2;
            if at(middle) < codepoint {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        (low < self.header.glyph_count as usize && at(low) == codepoint).then_some(low as u32)
    }

    pub(crate) fn glyph(&self, index: u32) -> GlyphRecord {
        let at = self.header.glyphs as usize + index as usize * GLYPH_SIZE;
        GlyphRecord::read(&self.data[at..at + GLYPH_SIZE])
    }

    /// Writes the glyph's A8 mask, `width` x `height` with no padding.
    pub(crate) fn decode(&self, index: u32, dst: &mut [u8]) {
        let glyph = self.glyph(index);
        let compressed = glyph.bitmap & GLYPH_COMPRESSED != 0;
        let start = self.header.data as usize + (glyph.bitmap & GLYPH_OFFSET_MASK) as usize;
        let end = self.header.data as usize + self.header.data_len as usize;
        debug_assert!(end - start >= FONT_DATA_SLACK);
        let bpp = self.header.bpp;
        let mut reader = LevelReader::new(&self.data[start..end], bpp, compressed);
        let prefilter = compressed && self.header.flags & FONT_PREFILTER != 0;
        let max = (1u32 << bpp) - 1;
        let width = usize::from(glyph.width);
        for i in 0..width * usize::from(glyph.height) {
            let mut level = reader.next();
            if prefilter && i >= width {
                level ^= opa_level(dst[i - width], max);
            }
            dst[i] = ((u32::from(level) * 255 + max / 2) / max) as u8;
        }
    }
}

// The row above, already expanded to 8 bits, back to its level.
#[inline]
fn opa_level(opa: u8, max: u32) -> u8 {
    ((u32::from(opa) * max + 127) / 255) as u8
}
