/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBounds;

impl fmt::Display for OutOfBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("out of bounds")
    }
}

impl core::error::Error for OutOfBounds {}

macro_rules! read_fns {
    ($($name:ident: $T:ty = $from:ident;)*) => {
        $(
            pub fn $name(&mut self) -> Result<$T, OutOfBounds> {
                Ok(<$T>::$from(self.array()?))
            }
        )*
    };
}

macro_rules! write_fns {
    ($($put:ident, $set:ident: $T:ty = $to:ident;)*) => {
        $(
            pub fn $put(&mut self, v: $T) -> Result<(), OutOfBounds> {
                self.put_bytes(&v.$to())
            }

            pub fn $set(&mut self, at: usize, v: $T) -> Result<(), OutOfBounds> {
                self.set_bytes(at, &v.$to())
            }
        )*
    };
}

pub struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub const fn position(&self) -> usize {
        self.pos
    }

    pub const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], OutOfBounds> {
        if n > self.remaining() {
            return Err(OutOfBounds);
        }
        let b = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], OutOfBounds> {
        let mut a = [0; N];
        a.copy_from_slice(self.bytes(N)?);
        Ok(a)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), OutOfBounds> {
        self.bytes(n).map(|_| ())
    }

    pub fn sub(&mut self, n: usize) -> Result<ByteReader<'a>, OutOfBounds> {
        self.bytes(n).map(ByteReader::new)
    }

    pub fn at(&self, offset: usize) -> Result<ByteReader<'a>, OutOfBounds> {
        match self.data.get(offset..) {
            Some(data) => Ok(ByteReader::new(data)),
            None => Err(OutOfBounds),
        }
    }

    pub fn u24_le(&mut self) -> Result<u32, OutOfBounds> {
        let [a, b, c] = self.array()?;
        Ok(u32::from_le_bytes([a, b, c, 0]))
    }

    pub fn u24_be(&mut self) -> Result<u32, OutOfBounds> {
        let [a, b, c] = self.array()?;
        Ok(u32::from_be_bytes([0, a, b, c]))
    }

    read_fns! {
        u8: u8 = from_le_bytes;
        i8: i8 = from_le_bytes;
        u16_le: u16 = from_le_bytes;
        u16_be: u16 = from_be_bytes;
        i16_le: i16 = from_le_bytes;
        i16_be: i16 = from_be_bytes;
        u32_le: u32 = from_le_bytes;
        u32_be: u32 = from_be_bytes;
        i32_le: i32 = from_le_bytes;
        i32_be: i32 = from_be_bytes;
        u64_le: u64 = from_le_bytes;
        u64_be: u64 = from_be_bytes;
        i64_le: i64 = from_le_bytes;
        i64_be: i64 = from_be_bytes;
        f32_le: f32 = from_le_bytes;
        f32_be: f32 = from_be_bytes;
        f64_le: f64 = from_le_bytes;
        f64_be: f64 = from_be_bytes;
    }
}

pub struct ByteWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> ByteWriter<'a> {
    pub const fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub const fn position(&self) -> usize {
        self.pos
    }

    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn written(&self) -> &[u8] {
        &self.buf[..self.pos]
    }

    pub fn into_written(self) -> &'a mut [u8] {
        &mut self.buf[..self.pos]
    }

    pub fn put_bytes(&mut self, b: &[u8]) -> Result<(), OutOfBounds> {
        self.fill_with(b.len(), |dst| dst.copy_from_slice(b))
    }

    pub fn fill(&mut self, n: usize, byte: u8) -> Result<(), OutOfBounds> {
        self.fill_with(n, |dst| dst.fill(byte))
    }

    pub fn reserve(&mut self, n: usize) -> Result<usize, OutOfBounds> {
        let at = self.pos;
        self.fill(n, 0)?;
        Ok(at)
    }

    pub fn set_bytes(&mut self, at: usize, b: &[u8]) -> Result<(), OutOfBounds> {
        match at.checked_add(b.len()) {
            Some(end) if end <= self.pos => {
                self.buf[at..end].copy_from_slice(b);
                Ok(())
            }
            _ => Err(OutOfBounds),
        }
    }

    fn fill_with(&mut self, n: usize, f: impl FnOnce(&mut [u8])) -> Result<(), OutOfBounds> {
        if n > self.remaining() {
            return Err(OutOfBounds);
        }
        f(&mut self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(())
    }

    // The u24 writers drop the high byte of v.
    pub fn put_u24_le(&mut self, v: u32) -> Result<(), OutOfBounds> {
        self.put_bytes(&v.to_le_bytes()[..3])
    }

    pub fn put_u24_be(&mut self, v: u32) -> Result<(), OutOfBounds> {
        self.put_bytes(&v.to_be_bytes()[1..])
    }

    pub fn set_u24_le(&mut self, at: usize, v: u32) -> Result<(), OutOfBounds> {
        self.set_bytes(at, &v.to_le_bytes()[..3])
    }

    pub fn set_u24_be(&mut self, at: usize, v: u32) -> Result<(), OutOfBounds> {
        self.set_bytes(at, &v.to_be_bytes()[1..])
    }

    write_fns! {
        put_u8, set_u8: u8 = to_le_bytes;
        put_i8, set_i8: i8 = to_le_bytes;
        put_u16_le, set_u16_le: u16 = to_le_bytes;
        put_u16_be, set_u16_be: u16 = to_be_bytes;
        put_i16_le, set_i16_le: i16 = to_le_bytes;
        put_i16_be, set_i16_be: i16 = to_be_bytes;
        put_u32_le, set_u32_le: u32 = to_le_bytes;
        put_u32_be, set_u32_be: u32 = to_be_bytes;
        put_i32_le, set_i32_le: i32 = to_le_bytes;
        put_i32_be, set_i32_be: i32 = to_be_bytes;
        put_u64_le, set_u64_le: u64 = to_le_bytes;
        put_u64_be, set_u64_be: u64 = to_be_bytes;
        put_i64_le, set_i64_le: i64 = to_le_bytes;
        put_i64_be, set_i64_be: i64 = to_be_bytes;
        put_f32_le, set_f32_le: f32 = to_le_bytes;
        put_f32_be, set_f32_be: f32 = to_be_bytes;
        put_f64_le, set_f64_le: f64 = to_le_bytes;
        put_f64_be, set_f64_be: f64 = to_be_bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteReader, ByteWriter, OutOfBounds};

    #[test]
    fn reads_in_order() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0xff];
        let mut r = ByteReader::new(&data);
        assert_eq!(r.u8(), Ok(0x01));
        assert_eq!(r.u16_le(), Ok(0x0302));
        assert_eq!(r.u16_be(), Ok(0x0405));
        assert_eq!(r.u24_le(), Ok(0x080706));
        assert_eq!(r.position(), 8);
        assert_eq!(r.remaining(), 2);
        assert_eq!(r.i16_le(), Ok(-247));
        assert!(r.is_empty());
        assert_eq!(r.u8(), Err(OutOfBounds));
    }

    #[test]
    fn reads_wide_and_float() {
        let mut data = [0u8; 40];
        data[..8].copy_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());
        data[8..16].copy_from_slice(&(-2i64).to_le_bytes());
        data[16..20].copy_from_slice(&1.5f32.to_be_bytes());
        data[20..28].copy_from_slice(&(-0.25f64).to_le_bytes());
        data[28..31].copy_from_slice(&[0x00, 0x12, 0x34]);
        data[31..35].copy_from_slice(&(-3i32).to_be_bytes());
        let mut r = ByteReader::new(&data);
        assert_eq!(r.u64_be(), Ok(0x0102_0304_0506_0708));
        assert_eq!(r.i64_le(), Ok(-2));
        assert_eq!(r.f32_be(), Ok(1.5));
        assert_eq!(r.f64_le(), Ok(-0.25));
        assert_eq!(r.u24_be(), Ok(0x1234));
        assert_eq!(r.i32_be(), Ok(-3));
    }

    #[test]
    fn failed_read_does_not_advance() {
        let data = [1, 2, 3];
        let mut r = ByteReader::new(&data);
        assert_eq!(r.u8(), Ok(1));
        assert_eq!(r.u32_le(), Err(OutOfBounds));
        assert_eq!(r.bytes(3), Err(OutOfBounds));
        assert_eq!(r.position(), 1);
        assert_eq!(r.u16_be(), Ok(0x0203));
    }

    #[test]
    fn slices_outlive_the_reader() {
        let data = *b"ftypisom\x00\x01";
        let (kind, rest) = {
            let mut r = ByteReader::new(&data);
            let kind: [u8; 4] = r.array().unwrap();
            r.skip(4).unwrap();
            (kind, r.rest())
        };
        assert_eq!(&kind, b"ftyp");
        assert_eq!(rest, [0, 1]);
    }

    #[test]
    fn sub_and_at() {
        let data = [0, 0, 0, 12, b'm', b'o', b'o', b'v', 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let mut r = ByteReader::new(&data);
        let size = r.u32_be().unwrap() as usize;
        assert_eq!(&r.array::<4>().unwrap(), b"moov");
        let mut body = r.sub(size - 8).unwrap();
        assert_eq!(body.remaining(), 4);
        assert_eq!(r.u8(), Ok(0xee));
        assert_eq!(body.at(2).unwrap().u16_be(), Ok(0xccdd));
        assert_eq!(body.at(4).unwrap().remaining(), 0);
        assert!(body.at(5).is_err());
        assert_eq!(body.u8(), Ok(0xaa));
        assert_eq!(body.at(0).unwrap().u8(), Ok(0xaa));
        assert!(r.sub(1).is_err());
    }

    #[test]
    fn writes_in_order() {
        let mut buf = [0u8; 16];
        let mut w = ByteWriter::new(&mut buf);
        w.put_u8(0x01).unwrap();
        w.put_u16_le(0x0302).unwrap();
        w.put_u24_be(0xff040506).unwrap();
        w.put_i16_be(-2).unwrap();
        w.put_f32_le(1.5).unwrap();
        w.fill(2, 0x7f).unwrap();
        assert_eq!(w.position(), 14);
        assert_eq!(w.remaining(), 2);
        let mut r = ByteReader::new(w.written());
        assert_eq!(r.u8(), Ok(0x01));
        assert_eq!(r.u16_le(), Ok(0x0302));
        assert_eq!(r.u24_be(), Ok(0x040506));
        assert_eq!(r.i16_be(), Ok(-2));
        assert_eq!(r.f32_le(), Ok(1.5));
        assert_eq!(r.rest(), [0x7f, 0x7f]);
    }

    #[test]
    fn failed_write_does_not_advance() {
        let mut buf = [0xeeu8; 3];
        let mut w = ByteWriter::new(&mut buf);
        w.put_u8(1).unwrap();
        assert_eq!(w.put_u32_be(0), Err(OutOfBounds));
        assert_eq!(w.put_bytes(&[0; 3]), Err(OutOfBounds));
        assert_eq!(w.fill(3, 0), Err(OutOfBounds));
        assert_eq!(w.position(), 1);
        w.put_u16_be(0x0203).unwrap();
        assert_eq!(w.into_written(), [1, 2, 3]);
    }

    #[test]
    fn reserve_and_set() {
        let mut buf = [0xeeu8; 16];
        let mut w = ByteWriter::new(&mut buf);
        let len_at = w.reserve(2).unwrap();
        w.put_u16_le(0x0004).unwrap();
        w.put_bytes(b"abc").unwrap();
        let len = (w.position() - len_at - 2) as u16;
        w.set_u16_le(len_at, len).unwrap();
        assert_eq!(w.written(), [5, 0, 4, 0, b'a', b'b', b'c']);
        assert_eq!(w.set_u32_be(4, 0), Err(OutOfBounds));
        assert_eq!(w.set_u8(usize::MAX, 0), Err(OutOfBounds));
        w.set_u24_be(4, 0x414243).unwrap();
        assert_eq!(&w.written()[4..], b"ABC");
    }
}
