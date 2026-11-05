/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use crate::{Error, Result};

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTF8_STRING: u8 = 0x0c;
pub const PRINTABLE_STRING: u8 = 0x13;
pub const IA5_STRING: u8 = 0x16;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// `[n]` with constructed content.
pub const fn context(n: u8) -> u8 {
    0xa0 | n
}

/// `[n]` with primitive content.
pub const fn context_primitive(n: u8) -> u8 {
    0x80 | n
}

/// Reads DER values in order. Long-form lengths up to 4 bytes; no indefinite lengths.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// The tag, the content and the whole encoding.
    pub fn read_any(&mut self) -> Result<(u8, &'a [u8], &'a [u8])> {
        let data = self.data;
        let (&tag, rest) = data.split_first().ok_or(Error::Decode)?;
        if tag & 0x1f == 0x1f {
            return Err(Error::Decode);
        }
        let (&first, mut rest) = rest.split_first().ok_or(Error::Decode)?;
        let len = if first < 0x80 {
            first as usize
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || rest.len() < n || rest[0] == 0 {
                return Err(Error::Decode);
            }
            let len = rest[..n].iter().fold(0usize, |acc, &b| acc << 8 | b as usize);
            if len < 0x80 {
                return Err(Error::Decode);
            }
            rest = &rest[n..];
            len
        };
        if rest.len() < len {
            return Err(Error::Decode);
        }
        let header = data.len() - rest.len();
        self.data = &rest[len..];
        Ok((tag, &rest[..len], &data[..header + len]))
    }

    pub fn read(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (t, content, _) = self.read_any()?;
        if t != tag {
            return Err(Error::Decode);
        }
        Ok(content)
    }

    /// The whole encoding, tag and length included.
    pub fn read_whole(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (t, _, whole) = self.read_any()?;
        if t != tag {
            return Err(Error::Decode);
        }
        Ok(whole)
    }

    pub fn read_optional(&mut self, tag: u8) -> Result<Option<&'a [u8]>> {
        if self.peek_tag() == Some(tag) { self.read(tag).map(Some) } else { Ok(None) }
    }

    pub fn sequence(&mut self) -> Result<Reader<'a>> {
        self.read(SEQUENCE).map(Reader::new)
    }

    pub fn nested(&mut self, tag: u8) -> Result<Reader<'a>> {
        self.read(tag).map(Reader::new)
    }

    pub fn boolean(&mut self) -> Result<bool> {
        match self.read(BOOLEAN)? {
            [0x00] => Ok(false),
            [0xff] => Ok(true),
            _ => Err(Error::Decode),
        }
    }

    /// A small non-negative INTEGER.
    pub fn small_unsigned(&mut self) -> Result<u32> {
        let value = unsigned(self.read(INTEGER)?)?;
        if value.len() > 4 {
            return Err(Error::Decode);
        }
        Ok(value.iter().fold(0u32, |acc, &b| acc << 8 | b as u32))
    }

    /// A BIT STRING with no unused bits.
    pub fn bit_string_bytes(&mut self) -> Result<&'a [u8]> {
        match self.read(BIT_STRING)? {
            [0, rest @ ..] => Ok(rest),
            _ => Err(Error::Decode),
        }
    }

    pub fn finish(&self) -> Result<()> {
        if self.data.is_empty() { Ok(()) } else { Err(Error::Decode) }
    }
}

/// The magnitude of a non-negative INTEGER, without its sign byte.
pub fn unsigned(content: &[u8]) -> Result<&[u8]> {
    match content {
        [] => Err(Error::Decode),
        [b, ..] if b & 0x80 != 0 => Err(Error::Decode),
        [0, rest @ ..] if rest.first().is_some_and(|b| b & 0x80 == 0) => Err(Error::Decode),
        [0, rest @ ..] if !rest.is_empty() => Ok(rest),
        _ => Ok(content),
    }
}

/// Builds DER; nested values are written in place and their lengths filled in after.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    pub fn tlv(&mut self, tag: u8, content: &[u8]) {
        self.buf.push(tag);
        push_len(&mut self.buf, content.len());
        self.buf.extend_from_slice(content);
    }

    pub fn raw(&mut self, encoded: &[u8]) {
        self.buf.extend_from_slice(encoded);
    }

    pub fn nested(&mut self, tag: u8, f: impl FnOnce(&mut Writer)) {
        let mut inner = Writer::new();
        f(&mut inner);
        self.tlv(tag, &inner.buf);
    }

    pub fn sequence(&mut self, f: impl FnOnce(&mut Writer)) {
        self.nested(SEQUENCE, f);
    }

    /// A non-negative INTEGER from big-endian magnitude bytes.
    pub fn unsigned_integer(&mut self, magnitude: &[u8]) {
        let start = magnitude.iter().position(|&b| b != 0).unwrap_or(magnitude.len().saturating_sub(1));
        let magnitude = if magnitude.is_empty() { &[0][..] } else { &magnitude[start..] };
        self.buf.push(INTEGER);
        if magnitude[0] & 0x80 != 0 {
            push_len(&mut self.buf, magnitude.len() + 1);
            self.buf.push(0);
        } else {
            push_len(&mut self.buf, magnitude.len());
        }
        self.buf.extend_from_slice(magnitude);
    }

    pub fn small_integer(&mut self, value: u32) {
        self.unsigned_integer(&value.to_be_bytes());
    }

    pub fn boolean(&mut self, value: bool) {
        self.tlv(BOOLEAN, &[if value { 0xff } else { 0 }]);
    }

    pub fn null(&mut self) {
        self.tlv(NULL, &[]);
    }

    pub fn oid(&mut self, encoded: &[u8]) {
        self.tlv(OID, encoded);
    }

    pub fn bit_string(&mut self, bytes: &[u8]) {
        self.buf.push(BIT_STRING);
        push_len(&mut self.buf, bytes.len() + 1);
        self.buf.push(0);
        self.buf.extend_from_slice(bytes);
    }
}

fn push_len(buf: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        buf.push(len as u8);
    } else {
        let bytes = (len as u32).to_be_bytes();
        let skip = bytes.iter().position(|&b| b != 0).unwrap();
        buf.push(0x80 | (4 - skip) as u8);
        buf.extend_from_slice(&bytes[skip..]);
    }
}
