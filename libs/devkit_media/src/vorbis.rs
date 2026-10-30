/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::buffer::{Aligned, Buffer};
use crate::tags::{COVER_MAX_BYTES, CoverAt, CoverFormat, Encoding, Field, Tags};

const PEEK_BYTES: usize = 256;
const SHORT_BYTES: usize = 16;

const KEYS: [(&str, Field); 7] = [
    ("TITLE", Field::Title),
    ("ARTIST", Field::Artist),
    ("ALBUM", Field::Album),
    ("ALBUMARTIST", Field::AlbumArtist),
    ("ALBUM ARTIST", Field::AlbumArtist),
    ("DATE", Field::Date),
    ("YEAR", Field::Date),
];

pub(crate) trait TagStream {
    fn read(&mut self, out: &mut [u8]) -> bool;
    fn skip(&mut self, size: usize) -> bool;
}

fn le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

fn key_is(key: &[u8], name: &str) -> bool {
    key.eq_ignore_ascii_case(name.as_bytes())
}

fn base64_value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn base64_decode(data: &mut [u8]) -> usize {
    let (mut bits, mut count, mut out) = (0u32, 0, 0);
    for i in 0..data.len() {
        let Some(value) = base64_value(data[i]) else {
            continue;
        };
        bits = bits << 6 | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            data[out] = (bits >> count) as u8;
            out += 1;
        }
    }
    out
}

fn adopt_picture_block(tags: &mut Tags, mut block: Aligned) {
    let size = block.len();
    if size < 32 {
        return;
    }
    let front = be32(&block) == 3;
    let mut at = 8 + be32(&block[4..]) as usize;
    if at + 4 > size {
        return;
    }
    at += 4 + be32(&block[at..]) as usize;
    if at + 20 > size {
        return;
    }
    let length = be32(&block[at + 16..]) as usize;
    at += 20;
    if length == 0 || length > size - at {
        return;
    }
    block.narrow(at, length);
    tags.adopt_cover(block, front);
}

struct FileStream<'a> {
    reader: &'a Buffer,
    at: i64,
}

impl TagStream for FileStream<'_> {
    fn read(&mut self, out: &mut [u8]) -> bool {
        let done = self.reader.read_exact_at(self.at, out);
        if done {
            self.at += out.len() as i64;
        }
        done
    }

    fn skip(&mut self, size: usize) -> bool {
        self.at += size as i64;
        true
    }
}

pub(crate) fn read_comment(reader: &Buffer, offset: i64, size: u32, tags: &mut Tags) -> bool {
    read_comment_stream(&mut FileStream { reader, at: offset }, size, tags, false)
}

// The leading digits of a C string, with what strtoul takes in front of them.
fn parse_number(text: &[u8]) -> u32 {
    let text = text.trim_ascii_start();
    let (negative, digits) = match text {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, text),
    };
    let value = digits
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u32, |v, &b| v.saturating_mul(10).saturating_add(u32::from(b - b'0')));
    if negative { value.wrapping_neg() } else { value }
}

fn up_to_nul(text: &[u8]) -> &[u8] {
    text.iter().position(|&b| b == 0).map_or(text, |end| &text[..end])
}

/// With `want_cover`, a METADATA_BLOCK_PICTURE (a base64 FLAC PICTURE block)
/// becomes the cover.
pub(crate) fn read_comment_stream(input: &mut dyn TagStream, size: u32, tags: &mut Tags, want_cover: bool) -> bool {
    let mut left = size;
    let mut word = [0; 4];
    if left < 8 || !input.read(&mut word) {
        return false;
    }
    left -= 4;
    let vendor = le32(&word);
    if vendor > left - 4 || !input.skip(vendor as usize) {
        return false;
    }
    left -= vendor;
    if !input.read(&mut word) {
        return false;
    }
    left -= 4;
    let mut count = le32(&word);

    let mut track = [0; SHORT_BYTES - 1];
    let mut track_len = 0;
    let mut total = 0;
    let mut text = [0; PEEK_BYTES];
    while count != 0 && left >= 4 {
        count -= 1;
        if !input.read(&mut word) {
            return false;
        }
        left -= 4;
        let len = le32(&word);
        if len > left {
            return false;
        }
        left -= len;
        let len = len as usize;
        let peek = len.min(PEEK_BYTES);
        if !input.read(&mut text[..peek]) {
            return false;
        }

        let eq = text[..peek].iter().position(|&b| b == b'=');
        let key_len = eq.unwrap_or(0);
        if eq.is_some() && key_is(&text[..key_len], "METADATA_BLOCK_PICTURE") {
            let encoded = len - key_len - 1;
            if want_cover && encoded / 4 * 3 <= COVER_MAX_BYTES + 1024 {
                if let Ok(mut block) = Aligned::new(0, encoded) {
                    let first = peek - key_len - 1;
                    block[..first].copy_from_slice(&text[key_len + 1..peek]);
                    if !input.read(&mut block[first..]) {
                        return false;
                    }
                    let decoded = base64_decode(&mut block);
                    block.shrink(decoded);
                    adopt_picture_block(tags, block);
                    continue;
                }
            }
        }
        if len > peek && !input.skip(len - peek) {
            return false;
        }
        if eq.is_none() {
            continue;
        }
        let key = &text[..key_len];
        let value = &text[key_len + 1..peek];
        if let Some(&(_, field)) = KEYS.iter().find(|(name, _)| key_is(key, name)) {
            tags.set(field, value, Encoding::Utf8);
        } else if key_is(key, "TRACKNUMBER") && up_to_nul(&track[..track_len]).is_empty() {
            track_len = value.len().min(track.len());
            track[..track_len].copy_from_slice(&value[..track_len]);
        } else if (key_is(key, "TRACKTOTAL") || key_is(key, "TOTALTRACKS")) && total == 0 {
            total = parse_number(up_to_nul(&value[..value.len().min(11)]));
        }
    }
    let track = up_to_nul(&track[..track_len]);
    if !track.is_empty() && total != 0 && !track.contains(&b'/') {
        tags.set_number(Field::Track, parse_number(track), total);
    } else if !track.is_empty() {
        tags.set(Field::Track, track, Encoding::Utf8);
    }
    true
}

pub(crate) fn read_flac_picture(reader: &Buffer, offset: i64, size: u32, tags: &mut Tags, want_cover: bool, have_front: &mut bool) -> bool {
    let end = offset + i64::from(size);
    let mut head = [0; 8];
    if size < 32 || !reader.read_exact_at(offset, &mut head) {
        return false;
    }
    let front = be32(&head) == 3;
    let mut at = offset + 8 + i64::from(be32(&head[4..]));
    if at + 4 > end || !reader.read_exact_at(at, &mut head[..4]) {
        return false;
    }
    at += 4 + i64::from(be32(&head));
    let mut dims = [0; 20];
    if at + 20 > end || !reader.read_exact_at(at, &mut dims) {
        return false;
    }
    at += 20;
    let length = be32(&dims[16..]);
    if length == 0 || i64::from(length) > end - at {
        return false;
    }

    let mut magic = [0; 8];
    if length >= 8 && reader.read_exact_at(at, &mut magic) && CoverFormat::of(&magic).is_none() {
        return true;
    }
    if tags.cover_at.is_some() && (!front || *have_front) {
        return true;
    }
    tags.cover_at = Some(CoverAt { offset: at, size: length });
    *have_front = front;
    if want_cover {
        if let Ok(data) = reader.read_alloc(at, length as usize) {
            tags.adopt_cover(data, front);
        }
    }
    true
}
