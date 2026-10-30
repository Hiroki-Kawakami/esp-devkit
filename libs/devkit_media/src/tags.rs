/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt::Write;

use devkit::FmtBuf;

use crate::buffer::Aligned;

pub const COVER_MAX_BYTES: usize = 2 * 1024 * 1024;

const TEXT_BYTES: usize = 128;
const SHORT_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Track,
    Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Latin1,
    Utf16,
    Utf16Be,
    Utf8,
}

/// Fixed-size UTF-8 of up to `N - 1` bytes.
#[derive(Clone, Copy)]
pub struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    const fn new() -> Self {
        Self { bytes: [0; N], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn put(&mut self, code: u32) -> bool {
        let code = if code < 0x20 || code == 0x7F { 0x20 } else { code };
        let Some(c) = char::from_u32(code) else {
            return false;
        };
        let bytes = c.len_utf8();
        if self.len + bytes + 1 > N {
            return false;
        }
        c.encode_utf8(&mut self.bytes[self.len..]);
        self.len += bytes;
        true
    }

    fn set(&mut self, bytes: &[u8], encoding: Encoding) {
        if !self.is_empty() || bytes.is_empty() {
            return;
        }
        match encoding {
            Encoding::Latin1 => self.convert_latin1(bytes),
            Encoding::Utf16 => self.convert_utf16(bytes, false),
            Encoding::Utf16Be => self.convert_utf16(bytes, true),
            Encoding::Utf8 => self.convert_utf8(bytes),
        }
        while self.len > 0 && self.bytes[self.len - 1] == b' ' {
            self.len -= 1;
        }
        let start = self.bytes[..self.len].iter().take_while(|&&b| b == b' ').count();
        self.bytes.copy_within(start..self.len, 0);
        self.len -= start;
    }

    fn convert_latin1(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == 0 || !self.put(u32::from(byte)) {
                return;
            }
        }
    }

    fn convert_utf16(&mut self, bytes: &[u8], big_endian: bool) {
        let (bytes, big_endian) = match bytes {
            [0xFF, 0xFE, rest @ ..] => (rest, false),
            [0xFE, 0xFF, rest @ ..] => (rest, true),
            _ => (bytes, big_endian),
        };
        let unit = |i: usize| {
            let pair = [bytes[i], bytes[i + 1]];
            u32::from(if big_endian { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) })
        };

        let mut i = 0;
        while i + 2 <= bytes.len() {
            let mut code = unit(i);
            if code == 0 {
                return;
            }
            if (0xD800..0xDC00).contains(&code) {
                if i + 4 > bytes.len() {
                    return;
                }
                let low = unit(i + 2);
                if !(0xDC00..=0xDFFF).contains(&low) {
                    return;
                }
                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                i += 2;
            } else if (0xDC00..=0xDFFF).contains(&code) {
                return;
            }
            if !self.put(code) {
                return;
            }
            i += 2;
        }
    }

    fn convert_utf8(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() && bytes[i] != 0 {
            let lead = bytes[i];
            let (length, mut code) = match lead {
                0x00..=0x7F => (1, u32::from(lead)),
                0xC0..=0xDF => (2, u32::from(lead & 0x1F)),
                0xE0..=0xEF => (3, u32::from(lead & 0x0F)),
                0xF0..=0xF7 => (4, u32::from(lead & 0x07)),
                _ => return,
            };
            if i + length > bytes.len() {
                return;
            }
            for &byte in &bytes[i + 1..i + length] {
                if byte & 0xC0 != 0x80 {
                    return;
                }
                code = code << 6 | u32::from(byte & 0x3F);
            }
            let shortest = match length {
                1 => 0,
                2 => 0x80,
                3 => 0x800,
                _ => 0x10000,
            };
            if code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code) || code < shortest {
                return;
            }
            if !self.put(code) {
                return;
            }
            i += length;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverFormat {
    Jpeg,
    Png,
}

impl CoverFormat {
    /// From the magic bytes, so a caller can tell a picture from any other
    /// payload.
    pub fn of(data: &[u8]) -> Option<Self> {
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(Self::Png)
        } else {
            None
        }
    }
}

pub struct Cover {
    data: Aligned,
    pub format: CoverFormat,
    pub front: bool,
}

impl core::ops::Deref for Cover {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

/// Where the picture sits in the file. Filled even when the body was skipped,
/// so a later pass can read just those bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverAt {
    pub offset: i64,
    pub size: u32,
}

/// What every demuxer fills. The first non-empty value of a field wins, so a
/// parser walks from the better source to the worse one.
pub struct Tags {
    pub title: Text<TEXT_BYTES>,
    pub artist: Text<TEXT_BYTES>,
    pub album: Text<TEXT_BYTES>,
    pub album_artist: Text<TEXT_BYTES>,
    pub track: Text<SHORT_BYTES>,
    pub date: Text<SHORT_BYTES>,
    pub cover: Option<Cover>,
    pub cover_at: Option<CoverAt>,
    /// The pictures were enumerated, so an empty `cover`/`cover_at` means the
    /// file has none rather than that nobody looked.
    pub cover_scanned: bool,
}

impl Default for Tags {
    fn default() -> Self {
        Self {
            title: Text::new(),
            artist: Text::new(),
            album: Text::new(),
            album_artist: Text::new(),
            track: Text::new(),
            date: Text::new(),
            cover: None,
            cover_at: None,
            cover_scanned: false,
        }
    }
}

impl Tags {
    pub fn get(&self, field: Field) -> &str {
        match field {
            Field::Title => self.title.as_str(),
            Field::Artist => self.artist.as_str(),
            Field::Album => self.album.as_str(),
            Field::AlbumArtist => self.album_artist.as_str(),
            Field::Track => self.track.as_str(),
            Field::Date => self.date.as_str(),
        }
    }

    pub fn is_empty(&self) -> bool {
        [Field::Title, Field::Artist, Field::Album, Field::AlbumArtist, Field::Track, Field::Date]
            .iter()
            .all(|&field| self.get(field).is_empty())
    }

    pub(crate) fn set(&mut self, field: Field, bytes: &[u8], encoding: Encoding) {
        match field {
            Field::Title => self.title.set(bytes, encoding),
            Field::Artist => self.artist.set(bytes, encoding),
            Field::Album => self.album.set(bytes, encoding),
            Field::AlbumArtist => self.album_artist.set(bytes, encoding),
            Field::Track => self.track.set(bytes, encoding),
            Field::Date => self.date.set(bytes, encoding),
        }
    }

    pub(crate) fn set_number(&mut self, field: Field, value: u32, total: u32) {
        if value == 0 || !self.get(field).is_empty() {
            return;
        }
        let mut text = FmtBuf::<SHORT_BYTES>::new();
        let _ = if total != 0 { write!(text, "{value}/{total}") } else { write!(text, "{value}") };
        self.set(field, text.as_str().as_bytes(), Encoding::Utf8);
    }

    fn keeps_cover_over(&self, front: bool) -> bool {
        self.cover.as_ref().is_some_and(|cover| !front || cover.front)
    }

    /// Takes the picture when it is a JPEG or a PNG within the size limit. A
    /// front cover replaces a picture stored without that hint.
    pub(crate) fn adopt_cover(&mut self, data: Aligned, front: bool) {
        if data.is_empty() || data.len() > COVER_MAX_BYTES || self.keeps_cover_over(front) {
            return;
        }
        if let Some(format) = CoverFormat::of(&data) {
            self.cover = Some(Cover { data, format, front });
        }
    }

    pub(crate) fn set_cover(&mut self, data: &[u8], front: bool) {
        if data.is_empty() || data.len() > COVER_MAX_BYTES || CoverFormat::of(data).is_none() || self.keeps_cover_over(front) {
            return;
        }
        if let Ok(mut copy) = Aligned::new(0, data.len()) {
            copy.copy_from_slice(data);
            self.adopt_cover(copy, front);
        }
    }
}
