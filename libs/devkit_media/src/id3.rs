/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::heap::{self, Caps};

use crate::buffer::{Buffer, psram_first};
use crate::tags::{CoverAt, CoverFormat, Encoding, Field, Tags};

const MAX_TAG_BYTES: usize = 4 * 1024 * 1024;
const HEADER_BYTES: usize = 10;
const V1_BYTES: i64 = 128;
// Enough for any text frame worth keeping and for the mime/description prefix
// of a picture frame.
const SCRATCH_BYTES: usize = 2048;

const FRAMES_3: [(&[u8; 4], Field); 7] = [
    (b"TIT2", Field::Title),
    (b"TPE1", Field::Artist),
    (b"TALB", Field::Album),
    (b"TPE2", Field::AlbumArtist),
    (b"TRCK", Field::Track),
    (b"TDRC", Field::Date),
    (b"TYER", Field::Date),
];

const FRAMES_2: [(&[u8; 3], Field); 6] = [
    (b"TT2", Field::Title),
    (b"TP1", Field::Artist),
    (b"TAL", Field::Album),
    (b"TP2", Field::AlbumArtist),
    (b"TRK", Field::Track),
    (b"TYE", Field::Date),
];

fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

fn synchsafe32(p: &[u8]) -> u32 {
    u32::from(p[0]) << 21 | u32::from(p[1]) << 14 | u32::from(p[2]) << 7 | u32::from(p[3])
}

fn frame_field(id: &[u8], major: u8) -> Option<Field> {
    if major == 2 {
        FRAMES_2.iter().find(|(name, _)| id.starts_with(*name)).map(|&(_, field)| field)
    } else {
        FRAMES_3.iter().find(|(name, _)| id.starts_with(*name)).map(|&(_, field)| field)
    }
}

fn is_picture(id: &[u8], major: u8) -> bool {
    id.starts_with(if major == 2 { b"PIC" } else { b"APIC" })
}

fn unsynchronise(data: &mut [u8]) -> usize {
    let size = data.len();
    let mut out = 0;
    let mut i = 0;
    while i < size {
        let byte = data[i];
        data[out] = byte;
        out += 1;
        if byte == 0xFF && i + 1 < size && data[i + 1] == 0 {
            i += 1;
        }
        i += 1;
    }
    out
}

fn text_encoding(code: u8) -> Option<Encoding> {
    match code {
        0 => Some(Encoding::Latin1),
        1 => Some(Encoding::Utf16),
        2 => Some(Encoding::Utf16Be),
        3 => Some(Encoding::Utf8),
        _ => None,
    }
}

fn skip_string(data: &[u8], wide: bool) -> usize {
    let found = if wide {
        data.chunks_exact(2).position(|pair| pair == [0, 0]).map(|i| i * 2 + 2)
    } else {
        data.iter().position(|&b| b == 0).map(|i| i + 1)
    };
    found.unwrap_or(data.len())
}

// Offset of the picture bytes inside an APIC/PIC body and its front-cover
// flag; `None` when the header before them does not parse, which includes it
// running past the prefix that was read.
fn picture_prefix(body: &[u8], v22: bool) -> Option<(usize, bool)> {
    let size = body.len();
    if size < 2 {
        return None;
    }
    let encoding = text_encoding(body[0])?;
    let wide = matches!(encoding, Encoding::Utf16 | Encoding::Utf16Be);

    let mut position = 1;
    position += if v22 { 3 } else { skip_string(&body[position..], false) };
    if position + 1 > size {
        return None;
    }
    let picture_type = body[position];
    position += 1;
    position += skip_string(&body[position..], wide);
    if position >= size {
        return None;
    }
    Some((position, picture_type == 3))
}

fn parse_picture(tags: &mut Tags, body: &[u8], v22: bool) {
    if let Some((prefix, front)) = picture_prefix(body, v22) {
        tags.set_cover(&body[prefix..], front);
    }
}

fn parse_frames(tags: &mut Tags, data: &mut [u8], major: u8) {
    let header_bytes = if major == 2 { 6 } else { 10 };
    let size = data.len();

    let mut position = 0;
    while position + header_bytes <= size {
        if data[position] == 0 {
            return;
        }
        let head = position;
        let (length, flags) = if major == 2 {
            (u32::from_be_bytes([0, data[head + 3], data[head + 4], data[head + 5]]) as usize, 0)
        } else {
            let field = &data[head + 4..];
            (if major == 4 { synchsafe32(field) } else { be32(field) } as usize, data[head + 9])
        };
        position += header_bytes;
        if length > size - position {
            return;
        }

        let (mut body_at, mut body_bytes) = (position, length);
        position += length;

        if major == 3 && flags & 0xC0 != 0 {
            continue;
        }
        if major == 3 && flags & 0x20 != 0 && body_bytes != 0 {
            body_at += 1;
            body_bytes -= 1;
        }
        if major == 4 {
            if flags & 0x0C != 0 {
                continue;
            }
            if flags & 0x40 != 0 && body_bytes != 0 {
                body_at += 1;
                body_bytes -= 1;
            }
            if flags & 0x01 != 0 && body_bytes >= 4 {
                body_at += 4;
                body_bytes -= 4;
            }
            if flags & 0x02 != 0 {
                body_bytes = unsynchronise(&mut data[body_at..body_at + body_bytes]);
            }
        }
        if body_bytes == 0 {
            continue;
        }

        let body = &data[body_at..body_at + body_bytes];
        let id = &data[head..head + 4];
        if is_picture(id, major) {
            parse_picture(tags, body, major == 2);
            continue;
        }
        if let (Some(field), Some(encoding)) = (frame_field(id, major), text_encoding(body[0])) {
            tags.set(field, &body[1..], encoding);
        }
    }
}

// Global unsynchronisation rewrites the byte stream, so frame lengths stop
// matching file offsets and the tag has to be taken in one piece.
fn read_whole(reader: &Buffer, body_at: i64, size: usize, flags: u8, major: u8, tags: &mut Tags) {
    let Ok(mut data) = reader.read_alloc(body_at, size) else {
        return;
    };
    let bytes = unsynchronise(&mut data);
    let mut start = 0;
    if flags & 0x40 != 0 {
        start = if bytes < 4 {
            bytes
        } else if major == 4 {
            synchsafe32(&data) as usize
        } else {
            be32(&data).wrapping_add(4) as usize
        };
    }
    if start < bytes {
        parse_frames(tags, &mut data[start..bytes], major);
    }
    tags.cover_scanned = true;
}

struct Walk<'a> {
    reader: &'a Buffer,
    tags: &'a mut Tags,
    want_cover: bool,
    have_front: bool,
    scratch: Box<[u8], Caps>,
}

impl Walk<'_> {
    fn handle_picture(&mut self, frame_at: i64, length: usize, v22: bool) {
        let peek = length.min(SCRATCH_BYTES);
        if !self.reader.read_exact_at(frame_at, &mut self.scratch[..peek]) {
            return;
        }
        let Some((prefix, front)) = picture_prefix(&self.scratch[..peek], v22) else {
            return;
        };
        if prefix >= length {
            return;
        }
        // Recording where a frame that is not a picture sits would send a
        // later pass off to read it for nothing.
        if prefix + 8 <= peek && CoverFormat::of(&self.scratch[prefix..prefix + 8]).is_none() {
            return;
        }
        if self.tags.cover_at.is_some() && (!front || self.have_front) {
            return;
        }

        let at = CoverAt { offset: frame_at + prefix as i64, size: (length - prefix) as u32 };
        self.tags.cover_at = Some(at);
        self.have_front = front;
        if self.want_cover {
            if let Ok(data) = self.reader.read_alloc(at.offset, at.size as usize) {
                self.tags.adopt_cover(data, front);
            }
        }
    }

    fn run(&mut self, body_at: i64, size: usize, flags: u8, major: u8) {
        let header_bytes = if major == 2 { 6 } else { 10 };
        let mut position = 0;
        if flags & 0x40 != 0 {
            let mut extended = [0; 4];
            if !self.reader.read_exact_at(body_at, &mut extended) {
                return;
            }
            position = if major == 4 { synchsafe32(&extended) } else { be32(&extended).wrapping_add(4) } as usize;
            if position >= size {
                return;
            }
        }

        while position + header_bytes <= size {
            let mut head = [0; 10];
            if !self.reader.read_exact_at(body_at + position as i64, &mut head[..header_bytes]) {
                break;
            }
            if head[0] == 0 {
                break;
            }
            let (mut length, frame_flags) = if major == 2 {
                (u32::from_be_bytes([0, head[3], head[4], head[5]]) as usize, 0)
            } else {
                (if major == 4 { synchsafe32(&head[4..]) } else { be32(&head[4..]) } as usize, head[9])
            };
            position += header_bytes;
            if length > size - position {
                break;
            }

            let mut frame_at = body_at + position as i64;
            position += length;
            if length == 0 {
                continue;
            }
            if major == 3 && frame_flags & 0xC0 != 0 {
                continue;
            }
            if major == 3 && frame_flags & 0x20 != 0 {
                frame_at += 1;
                length -= 1;
            }
            let mut unsynchronised = false;
            if major == 4 {
                if frame_flags & 0x0C != 0 {
                    continue;
                }
                if frame_flags & 0x40 != 0 {
                    frame_at += 1;
                    length -= 1;
                }
                if frame_flags & 0x01 != 0 && length >= 4 {
                    frame_at += 4;
                    length -= 4;
                }
                unsynchronised = frame_flags & 0x02 != 0;
            }
            if length == 0 {
                continue;
            }

            let picture = is_picture(&head, major);
            if picture && !unsynchronised {
                self.handle_picture(frame_at, length, major == 2);
                continue;
            }

            let field = frame_field(&head, major);
            if field.is_none() && !picture {
                continue;
            }
            let mut body_bytes = length.min(SCRATCH_BYTES);
            if !self.reader.read_exact_at(frame_at, &mut self.scratch[..body_bytes]) {
                break;
            }
            if unsynchronised {
                body_bytes = unsynchronise(&mut self.scratch[..body_bytes]);
            }
            if body_bytes == 0 {
                continue;
            }
            let body = &self.scratch[..body_bytes];
            if picture {
                parse_picture(self.tags, body, major == 2);
            } else if let (Some(field), Some(encoding)) = (field, text_encoding(body[0])) {
                self.tags.set(field, &body[1..], encoding);
            }
        }
        self.tags.cover_scanned = true;
    }
}

/// Reads the ID3v2 tag at `offset` and returns where it ends, or `None` when
/// there is none. With `want_cover` false the picture body is located but never
/// read, which is nearly the whole cost of a tag on a file with cover art.
pub(crate) fn read_v2(reader: &Buffer, offset: i64, tags: &mut Tags, want_cover: bool) -> Option<i64> {
    let mut header = [0; HEADER_BYTES];
    reader.seek(offset);
    if !reader.read_all(&mut header) || !header.starts_with(b"ID3") {
        return None;
    }
    if (header[6] | header[7] | header[8] | header[9]) & 0x80 != 0 {
        return None;
    }

    let major = header[3];
    let flags = header[5];
    let size = synchsafe32(&header[6..]) as usize;
    let end = offset + (HEADER_BYTES + size) as i64 + if flags & 0x10 != 0 { 10 } else { 0 };
    if size == 0 || size > MAX_TAG_BYTES || !(2..=4).contains(&major) {
        return Some(end);
    }

    let body_at = offset + HEADER_BYTES as i64;
    if flags & 0x80 != 0 {
        read_whole(reader, body_at, size, flags, major, tags);
    } else if let Ok(scratch) = psram_first(|caps| heap::zeroed_slice(SCRATCH_BYTES, caps)) {
        Walk { reader, tags, want_cover, have_front: false, scratch }.run(body_at, size, flags, major);
    }
    Some(end)
}

pub(crate) fn read_v1(reader: &Buffer, end: i64, tags: &mut Tags) -> bool {
    let mut block = [0; V1_BYTES as usize];
    if end < V1_BYTES || !reader.read_exact_at(end - V1_BYTES, &mut block) || !block.starts_with(b"TAG") {
        return false;
    }
    tags.set(Field::Title, &block[3..33], Encoding::Latin1);
    tags.set(Field::Artist, &block[33..63], Encoding::Latin1);
    tags.set(Field::Album, &block[63..93], Encoding::Latin1);
    tags.set(Field::Date, &block[93..97], Encoding::Latin1);
    if block[125] == 0 && block[126] != 0 {
        tags.set_number(Field::Track, u32::from(block[126]), 0);
    }
    true
}
