/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! HPACK (RFC 7541): a decoder with the dynamic table, and an encoder that
//! never indexes.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{Error, Result};

const STATIC: [(&str, &str); 61] = [
    (":authority", ""),
    (":method", "GET"),
    (":method", "POST"),
    (":path", "/"),
    (":path", "/index.html"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "200"),
    (":status", "204"),
    (":status", "206"),
    (":status", "304"),
    (":status", "400"),
    (":status", "404"),
    (":status", "500"),
    ("accept-charset", ""),
    ("accept-encoding", "gzip, deflate"),
    ("accept-language", ""),
    ("accept-ranges", ""),
    ("accept", ""),
    ("access-control-allow-origin", ""),
    ("age", ""),
    ("allow", ""),
    ("authorization", ""),
    ("cache-control", ""),
    ("content-disposition", ""),
    ("content-encoding", ""),
    ("content-language", ""),
    ("content-length", ""),
    ("content-location", ""),
    ("content-range", ""),
    ("content-type", ""),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("expect", ""),
    ("expires", ""),
    ("from", ""),
    ("host", ""),
    ("if-match", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("if-range", ""),
    ("if-unmodified-since", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("max-forwards", ""),
    ("proxy-authenticate", ""),
    ("proxy-authorization", ""),
    ("range", ""),
    ("referer", ""),
    ("refresh", ""),
    ("retry-after", ""),
    ("server", ""),
    ("set-cookie", ""),
    ("strict-transport-security", ""),
    ("transfer-encoding", ""),
    ("user-agent", ""),
    ("vary", ""),
    ("via", ""),
    ("www-authenticate", ""),
];

/// Codes of each length, 0 to 30 bits; the code is canonical.
const HUFFMAN_COUNTS: [u16; 31] =
    [0, 0, 0, 0, 0, 10, 26, 32, 6, 0, 5, 3, 2, 6, 2, 3, 0, 0, 0, 3, 8, 13, 26, 29, 12, 4, 15, 19, 29, 0, 4];

/// Symbols in the order of their codes; 256 is EOS.
const HUFFMAN_SYMBOLS: [u16; 257] = [
    48, 49, 50, 97, 99, 101, 105, 111, 115, 116, 32, 37, 45, 46, 47, 51, 52, 53, 54, 55, 56, 57, 61, 65, 95, 98, 100,
    102, 103, 104, 108, 109, 110, 112, 114, 117, 58, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82,
    83, 84, 85, 86, 87, 89, 106, 107, 113, 118, 119, 120, 121, 122, 38, 42, 44, 59, 88, 90, 33, 34, 40, 41, 63, 39, 43,
    124, 35, 62, 0, 36, 64, 91, 93, 126, 94, 125, 60, 96, 123, 92, 195, 208, 128, 130, 131, 162, 184, 194, 224, 226,
    153, 161, 167, 172, 176, 177, 179, 209, 216, 217, 227, 229, 230, 129, 132, 133, 134, 136, 146, 154, 156, 160, 163,
    164, 169, 170, 173, 178, 181, 185, 186, 187, 189, 190, 196, 198, 228, 232, 233, 1, 135, 137, 138, 139, 140, 141, 143,
    147, 149, 150, 151, 152, 155, 157, 158, 165, 166, 168, 174, 175, 180, 182, 183, 188, 191, 197, 231, 239, 9, 142, 144,
    145, 148, 159, 171, 206, 215, 225, 236, 237, 199, 207, 234, 235, 192, 193, 200, 201, 202, 205, 210, 213, 218, 219,
    238, 240, 242, 243, 255, 203, 204, 211, 212, 214, 221, 222, 223, 241, 244, 245, 246, 247, 248, 250, 251, 252, 253,
    254, 2, 3, 4, 5, 6, 7, 8, 11, 12, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 30, 31, 127, 220, 249,
    10, 13, 22, 256,
];

const EOS: u16 = 256;
const ENTRY_OVERHEAD: usize = 32;

fn huffman(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 8 / 5);
    let (mut code, mut first, mut index, mut len) = (0i32, 0i32, 0i32, 0usize);
    for &byte in data {
        for bit in (0..8).rev() {
            code |= i32::from((byte >> bit) & 1);
            len += 1;
            let count = i32::from(*HUFFMAN_COUNTS.get(len).ok_or(Error::InvalidData)?);
            if code - first < count {
                let sym = HUFFMAN_SYMBOLS[(index + code - first) as usize];
                if sym == EOS {
                    return Err(Error::InvalidData);
                }
                out.push(sym as u8);
                (code, first, index, len) = (0, 0, 0, 0);
                continue;
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
    }
    // What is left must be the start of EOS, all ones, shorter than a byte.
    if len > 7 || (code >> 1) != (1 << len) - 1 {
        return Err(Error::InvalidData);
    }
    Ok(out)
}

fn integer(block: &[u8], pos: &mut usize, prefix: u32) -> Result<usize> {
    let mask = (1usize << prefix) - 1;
    let mut value = usize::from(*block.get(*pos).ok_or(Error::InvalidData)?) & mask;
    *pos += 1;
    if value < mask {
        return Ok(value);
    }
    let mut shift = 0;
    loop {
        let b = *block.get(*pos).ok_or(Error::InvalidData)?;
        *pos += 1;
        value = value.checked_add(usize::from(b & 0x7F) << shift).ok_or(Error::InvalidData)?;
        if b & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 28 {
            return Err(Error::InvalidData);
        }
    }
}

fn string(block: &[u8], pos: &mut usize) -> Result<String> {
    let coded = block.get(*pos).ok_or(Error::InvalidData)? & 0x80 != 0;
    let len = integer(block, pos, 7)?;
    let raw = block.get(*pos..*pos + len).ok_or(Error::InvalidData)?;
    *pos += len;
    let bytes = if coded { huffman(raw)? } else { raw.to_vec() };
    Ok(String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

pub(crate) struct Decoder {
    table: VecDeque<(String, String)>,
    size: usize,
    max: usize,
    /// What SETTINGS_HEADER_TABLE_SIZE allows.
    limit: usize,
}

impl Decoder {
    pub fn new(limit: usize) -> Self {
        Self { table: VecDeque::new(), size: 0, max: limit, limit }
    }

    fn entry(&self, index: usize) -> Result<(String, String)> {
        let (name, value) = match index {
            0 => return Err(Error::InvalidData),
            1..=61 => STATIC[index - 1],
            _ => {
                let (n, v) = self.table.get(index - 62).ok_or(Error::InvalidData)?;
                return Ok((n.clone(), v.clone()));
            }
        };
        Ok((String::from(name), String::from(value)))
    }

    fn evict(&mut self, room: usize) {
        while self.size + room > self.max {
            let Some((n, v)) = self.table.pop_back() else { return };
            self.size -= n.len() + v.len() + ENTRY_OVERHEAD;
        }
    }

    fn insert(&mut self, name: &str, value: &str) {
        let size = name.len() + value.len() + ENTRY_OVERHEAD;
        self.evict(size);
        if size <= self.max {
            self.table.push_front((String::from(name), String::from(value)));
            self.size += size;
        }
    }

    /// The fields of one header block; more than `max_list` bytes of them,
    /// counted as SETTINGS_MAX_HEADER_LIST_SIZE counts, fail.
    pub fn decode(&mut self, block: &[u8], max_list: usize) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut listed = 0;
        let mut pos = 0;
        while pos < block.len() {
            let b = block[pos];
            let (name, value) = if b & 0x80 != 0 {
                self.entry(integer(block, &mut pos, 7)?)?
            } else if b & 0xE0 == 0x20 {
                if !out.is_empty() {
                    return Err(Error::InvalidData);
                }
                let max = integer(block, &mut pos, 5)?;
                if max > self.limit {
                    return Err(Error::InvalidData);
                }
                self.max = max;
                self.evict(0);
                continue;
            } else {
                let indexing = b & 0xC0 == 0x40;
                let index = integer(block, &mut pos, if indexing { 6 } else { 4 })?;
                let name = if index == 0 { string(block, &mut pos)? } else { self.entry(index)?.0 };
                let value = string(block, &mut pos)?;
                if indexing {
                    self.insert(&name, &value);
                }
                (name, value)
            };
            listed += name.len() + value.len() + ENTRY_OVERHEAD;
            if listed > max_list {
                return Err(Error::MessageTooLarge);
            }
            out.push((name, value));
        }
        Ok(out)
    }
}

fn put_integer(out: &mut Vec<u8>, first: u8, prefix: u32, mut value: usize) {
    let mask = (1usize << prefix) - 1;
    if value < mask {
        out.push(first | value as u8);
        return;
    }
    out.push(first | mask as u8);
    value -= mask;
    while value >= 0x80 {
        out.push((value as u8 & 0x7F) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    put_integer(out, 0, 7, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// Appends a field: indexed when the static table has it whole, else a
/// literal that is not indexed, never-indexed for credentials. `name` is
/// lowercase.
pub(crate) fn encode(out: &mut Vec<u8>, name: &str, value: &str) {
    if let Some(i) = STATIC.iter().position(|&(n, v)| n == name && v == value && !v.is_empty()) {
        put_integer(out, 0x80, 7, i + 1);
        return;
    }
    let first = if matches!(name, "authorization" | "proxy-authorization" | "cookie") { 0x10 } else { 0x00 };
    match STATIC.iter().position(|&(n, _)| n == name) {
        Some(i) => put_integer(out, first, 4, i + 1),
        None => {
            put_integer(out, first, 4, 0);
            put_string(out, name);
        }
    }
    put_string(out, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn fields(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|&(n, v)| (String::from(n), String::from(v))).collect()
    }

    #[test]
    fn decodes_huffman_and_the_dynamic_table() {
        let mut d = Decoder::new(4096);
        let date = "Mon, 21 Oct 2013 20:13:21 GMT";
        let first = hex(
            "3fe101488264025885aec3771a4b6196d07abe941054d444a8200595040b8166e082a62d1bff6e919d29ad171863c78f0b97c8e9ae82ae43d3",
        );
        let want = [(":status", "302"), ("cache-control", "private"), ("date", date), ("location", "https://www.example.com")];
        assert_eq!(d.decode(&first, 1 << 16).unwrap(), fields(&want));
        assert_eq!(d.max, 256);
        let want = [(":status", "307"), ("cache-control", "private"), ("date", date), ("location", "https://www.example.com")];
        assert_eq!(d.decode(&hex("4883640effc1c0bf"), 1 << 16).unwrap(), fields(&want));
        let third = hex(
            "88c16196d07abe941054d444a8200595040b8166e084a62d1bffc05a839bd9ab77ad94e7821dd7f2e6c7b335dfdfcd5b3960d5af27087f3672c1ab270fb5291f9587316065c003ed4ee5b1063d5007",
        );
        let want = [
            (":status", "200"),
            ("cache-control", "private"),
            ("date", "Mon, 21 Oct 2013 20:13:22 GMT"),
            ("location", "https://www.example.com"),
            ("content-encoding", "gzip"),
            ("set-cookie", "foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1"),
        ];
        assert_eq!(d.decode(&third, 1 << 16).unwrap(), fields(&want));
        assert!(d.size <= 256);
        let fourth = hex("881f0884414961534086f2b47e92547f8d20e5fffe3fffeea9ffffff9ffd");
        let want = [(":status", "200"), ("authorization", "secret"), ("x-bytes", "caf\u{e9} \u{7f}~")];
        assert_eq!(d.decode(&fourth, 1 << 16).unwrap(), fields(&want));
    }

    #[test]
    fn rejects_what_does_not_decode() {
        let mut d = Decoder::new(4096);
        assert!(d.decode(&[0xBF], 1 << 16).is_err(), "index past the tables");
        assert!(d.decode(&[0x3F, 0xE2, 0x1F], 1 << 16).is_err(), "table size over the limit");
        assert!(d.decode(&[0x82, 0x20], 1 << 16).is_err(), "table size after a field");
        assert!(d.decode(&[0x04, 0x81, 0x00], 1 << 16).is_err(), "padding that is not ones");
        assert!(d.decode(&[0x04, 0x84, 0xFF, 0xFF, 0xFF, 0xFF], 1 << 16).is_err(), "EOS in a string");
        assert!(d.decode(&[0x04, 0x85, 0xFF], 1 << 16).is_err(), "string past the block");
        assert_eq!(d.decode(&[0x82, 0x84], 70).err(), Some(Error::MessageTooLarge));
    }

    #[test]
    fn encodes_what_the_decoder_reads() {
        let mut block = Vec::new();
        let list = [
            (":method", "GET"),
            (":path", "/a"),
            (":authority", "h"),
            ("accept-encoding", "gzip, deflate"),
            ("authorization", "Basic x"),
            ("x-long", &"v".repeat(300)),
        ];
        for (n, v) in list {
            encode(&mut block, n, v);
        }
        assert_eq!(block[..2], [0x82, 0x04]);
        let mut d = Decoder::new(4096);
        assert_eq!(d.decode(&block, 1 << 16).unwrap(), fields(&list));
        assert!(d.table.is_empty());
    }
}
