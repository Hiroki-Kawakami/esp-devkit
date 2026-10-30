/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Resource binaries, written by `devkit resgen` and read by this crate. Layout
 * in docs/resgen.md.
 */

use alloc::vec::Vec;

pub const VERSION: u16 = 1;
pub const FONT_MAGIC: [u8; 4] = *b"DKFN";
pub const IMAGE_MAGIC: [u8; 4] = *b"DKIM";
pub const FONT_HEADER_SIZE: usize = 40;
pub const IMAGE_HEADER_SIZE: usize = 20;
pub const IMAGE_DATA_OFFSET: usize = 64;
pub const GLYPH_SIZE: usize = 12;
pub const GLYPH_COMPRESSED: u32 = 1 << 31;
pub const GLYPH_OFFSET_MASK: u32 = GLYPH_COMPRESSED - 1;
pub const FONT_PREFILTER: u8 = 1;
/// The decoder reads a 24 bit window, so glyph data ends with this many zeros.
pub const FONT_DATA_SLACK: usize = 2;

const REPEATS_BEFORE_COUNT: u32 = 11;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ImageFormat {
    Rgb565 = 1,
    /// B, G, R in memory.
    Rgb888 = 2,
    /// B, G, R, A in memory, not premultiplied.
    Argb8888 = 3,
    A8 = 4,
    L8 = 5,
}

impl ImageFormat {
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Rgb565),
            2 => Some(Self::Rgb888),
            3 => Some(Self::Argb8888),
            4 => Some(Self::A8),
            5 => Some(Self::L8),
            _ => None,
        }
    }

    pub const fn bytes(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
            Self::Argb8888 => 4,
            Self::A8 | Self::L8 => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FontHeader {
    pub bpp: u8,
    pub flags: u8,
    pub px: u16,
    pub line_height: i16,
    pub base_line: i16,
    pub max_ascent: i16,
    pub max_descent: i16,
    pub underline_position: i8,
    pub underline_thickness: u8,
    pub glyph_count: u32,
    pub codepoints: u32,
    pub glyphs: u32,
    pub data: u32,
    pub data_len: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GlyphRecord {
    /// Byte offset into the glyph data, with `GLYPH_COMPRESSED` when RLE.
    pub bitmap: u32,
    /// In 1/16 px.
    pub advance: u16,
    pub width: u8,
    pub height: u8,
    pub ofs_x: i8,
    pub ofs_y: i8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageHeader {
    pub format: ImageFormat,
    pub width: u16,
    pub height: u16,
    pub stride: u32,
    pub data_len: u32,
}

const fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

const fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

const fn magic_at(data: &[u8], magic: [u8; 4]) -> bool {
    data[0] == magic[0] && data[1] == magic[1] && data[2] == magic[2] && data[3] == magic[3]
}

impl FontHeader {
    pub const fn read(data: &[u8]) -> Option<Self> {
        if data.len() < FONT_HEADER_SIZE || !magic_at(data, FONT_MAGIC) || u16_at(data, 4) != VERSION {
            return None;
        }
        Some(Self {
            bpp: data[6],
            flags: data[7],
            px: u16_at(data, 8),
            line_height: u16_at(data, 10) as i16,
            base_line: u16_at(data, 12) as i16,
            max_ascent: u16_at(data, 14) as i16,
            max_descent: u16_at(data, 16) as i16,
            underline_position: data[18] as i8,
            underline_thickness: data[19],
            glyph_count: u32_at(data, 20),
            codepoints: u32_at(data, 24),
            glyphs: u32_at(data, 28),
            data: u32_at(data, 32),
            data_len: u32_at(data, 36),
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&FONT_MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&[self.bpp, self.flags]);
        out.extend_from_slice(&self.px.to_le_bytes());
        for v in [self.line_height, self.base_line, self.max_ascent, self.max_descent] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[self.underline_position as u8, self.underline_thickness]);
        for v in [self.glyph_count, self.codepoints, self.glyphs, self.data, self.data_len] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

impl GlyphRecord {
    pub fn read(data: &[u8]) -> Self {
        Self {
            bitmap: u32_at(data, 0),
            advance: u16_at(data, 4),
            width: data[6],
            height: data[7],
            ofs_x: data[8] as i8,
            ofs_y: data[9] as i8,
        }
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.bitmap.to_le_bytes());
        out.extend_from_slice(&self.advance.to_le_bytes());
        out.extend_from_slice(&[self.width, self.height, self.ofs_x as u8, self.ofs_y as u8, 0, 0]);
    }
}

impl ImageHeader {
    pub const fn read(data: &[u8]) -> Option<Self> {
        if data.len() < IMAGE_HEADER_SIZE || !magic_at(data, IMAGE_MAGIC) || u16_at(data, 4) != VERSION {
            return None;
        }
        let Some(format) = ImageFormat::from_raw(data[6]) else { return None };
        Some(Self {
            format,
            width: u16_at(data, 8),
            height: u16_at(data, 10),
            stride: u32_at(data, 12),
            data_len: u32_at(data, 16),
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&IMAGE_MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&[self.format as u8, 0]);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.stride.to_le_bytes());
        out.extend_from_slice(&self.data_len.to_le_bytes());
    }
}

pub fn pack_levels(levels: &[u8], bpp: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((levels.len() * usize::from(bpp)).div_ceil(8));
    let mut bits = BitWriter::new(&mut out);
    for &level in levels {
        bits.put(u32::from(level), u32::from(bpp));
    }
    bits.finish();
    out
}

/// Every row XORed with the row above, for `rle_encode`.
pub fn prefilter_rows(levels: &[u8], width: usize) -> Vec<u8> {
    let mut out = Vec::from(levels);
    for i in (width..out.len()).rev() {
        out[i] ^= levels[i - width];
    }
    out
}

/// A value costs `bpp` bits. A value equal to the previous one switches to
/// repeat mode, where each further repeat is a 1 bit, a 0 bit ends the run, and
/// the 11th repeat is followed by a 6 bit count. The count runs out on a value,
/// not on a repeat, and the decoder does not re-enter repeat mode on that value.
pub fn rle_encode(levels: &[u8], bpp: u8) -> Vec<u8> {
    let bpp = u32::from(bpp);
    let mut out = Vec::new();
    let mut bits = BitWriter::new(&mut out);
    let total = levels.len();
    let mut index = 0;
    let mut previous = None;
    let mut repeats = 0;
    let mut repeating = false;
    while index < total {
        if !repeating {
            let value = levels[index];
            bits.put(u32::from(value), bpp);
            index += 1;
            repeating = previous == Some(value);
            repeats = 0;
            previous = Some(value);
        } else if Some(levels[index]) == previous {
            bits.put(1, 1);
            repeats += 1;
            index += 1;
            if repeats == REPEATS_BEFORE_COUNT {
                let run = levels[index..].iter().take_while(|&&v| Some(v) == previous).count();
                let length = (run + 1).min(63);
                bits.put(length as u32, 6);
                index += length - 1;
                if index < total {
                    previous = Some(levels[index]);
                    bits.put(u32::from(levels[index]), bpp);
                    index += 1;
                }
                repeating = false;
            }
        } else {
            bits.put(0, 1);
            repeating = false;
        }
    }
    bits.finish();
    out
}

struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u32,
    bits: u32,
}

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out, acc: 0, bits: 0 }
    }

    fn put(&mut self, value: u32, count: u32) {
        self.acc = self.acc << count | value;
        self.bits += count;
        while self.bits >= 8 {
            self.bits -= 8;
            self.out.push((self.acc >> self.bits) as u8);
        }
    }

    fn finish(&mut self) {
        if self.bits > 0 {
            self.out.push((self.acc << (8 - self.bits)) as u8);
            self.bits = 0;
        }
    }
}

#[derive(Clone, Copy)]
enum RleState {
    Single,
    Repeat,
    Count,
}

/// Reads the levels of one glyph. `data` must extend `FONT_DATA_SLACK` bytes
/// past the glyph.
pub struct LevelReader<'a> {
    data: &'a [u8],
    position: usize,
    bpp: u32,
    compressed: bool,
    started: bool,
    previous: u8,
    repeats: u32,
    state: RleState,
}

impl<'a> LevelReader<'a> {
    pub fn new(data: &'a [u8], bpp: u8, compressed: bool) -> Self {
        Self {
            data,
            position: 0,
            bpp: u32::from(bpp),
            compressed,
            started: false,
            previous: 0,
            repeats: 0,
            state: RleState::Single,
        }
    }

    fn bits(&mut self, count: u32) -> u32 {
        let byte = self.position >> 3;
        let offset = (self.position & 7) as u32;
        let window = u32::from(self.data[byte]) << 16
            | u32::from(self.data[byte + 1]) << 8
            | u32::from(self.data[byte + 2]);
        self.position += count as usize;
        (window >> (24 - offset - count)) & ((1 << count) - 1)
    }

    fn single(&mut self) -> u8 {
        self.previous = self.bits(self.bpp) as u8;
        self.state = RleState::Single;
        self.previous
    }

    pub fn next(&mut self) -> u8 {
        if !self.compressed {
            return self.bits(self.bpp) as u8;
        }
        match self.state {
            RleState::Single => {
                let value = self.bits(self.bpp) as u8;
                if self.started && value == self.previous {
                    self.state = RleState::Repeat;
                    self.repeats = 0;
                }
                self.started = true;
                self.previous = value;
                value
            }
            RleState::Repeat => {
                self.repeats += 1;
                if self.bits(1) == 1 {
                    if self.repeats != REPEATS_BEFORE_COUNT {
                        return self.previous;
                    }
                    self.repeats = self.bits(6);
                    if self.repeats != 0 {
                        self.state = RleState::Count;
                        return self.previous;
                    }
                }
                self.single()
            }
            RleState::Count => {
                self.repeats -= 1;
                if self.repeats != 0 {
                    return self.previous;
                }
                self.single()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn roundtrip(levels: &[u8], bpp: u8) {
        let mut encoded = rle_encode(levels, bpp);
        encoded.extend_from_slice(&[0; FONT_DATA_SLACK]);
        let mut reader = LevelReader::new(&encoded, bpp, true);
        let decoded: Vec<u8> = levels.iter().map(|_| reader.next()).collect();
        assert_eq!(decoded, levels);
    }

    #[test]
    fn rle_roundtrips_runs_around_the_count() {
        for run in [1, 2, 3, 10, 11, 12, 13, 14, 15, 63, 64, 70, 200] {
            for tail in [0, 1, 2] {
                let mut levels = vec![1u8; run];
                levels.extend(core::iter::repeat_n(2u8, tail));
                roundtrip(&levels, 2);
                roundtrip(&levels, 4);
            }
        }
    }

    #[test]
    fn rle_roundtrips_mixed_levels() {
        let levels: Vec<u8> = (0..4096u32).map(|i| ((i * i / 97 + i / 13) % 16) as u8).collect();
        roundtrip(&levels, 4);
        let two: Vec<u8> = levels.iter().map(|v| v >> 2).collect();
        roundtrip(&two, 2);
        let one: Vec<u8> = levels.iter().map(|v| v >> 3).collect();
        roundtrip(&one, 1);
    }

    #[test]
    fn prefilter_is_undone_by_xoring_the_row_above() {
        let levels = [1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        let filtered = prefilter_rows(&levels, 3);
        assert_eq!(&filtered[..3], &levels[..3]);
        assert_eq!(filtered[3], 4 ^ 1);
        assert_eq!(filtered[8], 9 ^ 6);
    }

    #[test]
    fn packed_levels_read_back() {
        let levels = [0u8, 1, 2, 3, 3, 2, 1];
        let mut packed = pack_levels(&levels, 2);
        packed.extend_from_slice(&[0; FONT_DATA_SLACK]);
        let mut reader = LevelReader::new(&packed, 2, false);
        for level in levels {
            assert_eq!(reader.next(), level);
        }
    }

    #[test]
    fn headers_read_back() {
        let font = FontHeader {
            bpp: 4,
            flags: FONT_PREFILTER,
            px: 24,
            line_height: 29,
            base_line: 6,
            max_ascent: 23,
            max_descent: -5,
            underline_position: -3,
            underline_thickness: 1,
            glyph_count: 3,
            codepoints: 40,
            glyphs: 52,
            data: 88,
            data_len: 100,
        };
        let mut out = Vec::new();
        font.write(&mut out);
        assert_eq!(out.len(), FONT_HEADER_SIZE);
        assert_eq!(FontHeader::read(&out), Some(font));

        let image = ImageHeader { format: ImageFormat::Rgb888, width: 7, height: 9, stride: 21, data_len: 189 };
        let mut out = Vec::new();
        image.write(&mut out);
        assert_eq!(out.len(), IMAGE_HEADER_SIZE);
        assert_eq!(ImageHeader::read(&out), Some(image));
        out[0] = 0;
        assert_eq!(ImageHeader::read(&out), None);

        let glyph = GlyphRecord { bitmap: GLYPH_COMPRESSED | 5, advance: 160, width: 9, height: 8, ofs_x: -1, ofs_y: -2 };
        let mut out = Vec::new();
        glyph.write(&mut out);
        assert_eq!(out.len(), GLYPH_SIZE);
        assert_eq!(GlyphRecord::read(&out), glyph);
    }
}
