/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;

const WT_VARINT: u8 = 0;
const WT_I64: u8 = 1;
const WT_LEN: u8 = 2;
const WT_I32: u8 = 5;

const MAX_DEPTH: u8 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DecodeError;

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("malformed protobuf message")
    }
}

pub trait Encode {
    fn encoded_len(&self) -> usize;
    fn encode_fields(&self, w: &mut Writer<'_>);

    /// `out` must hold `encoded_len()` bytes; returns that length.
    fn encode(&self, out: &mut [u8]) -> usize {
        let mut w = Writer { buf: out, pos: 0 };
        self.encode_fields(&mut w);
        w.pos
    }

    fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = alloc::vec![0; self.encoded_len()];
        self.encode(&mut out);
        out
    }
}

/// Bytes fields borrow from the input.
pub trait Message<'a>: Encode + Default {
    fn merge_field(&mut self, field: Field<'a>) -> Result<(), DecodeError>;

    fn decode(data: &'a [u8]) -> Result<Self, DecodeError> {
        decode_at(data, 0)
    }
}

fn decode_at<'a, T: Message<'a>>(data: &'a [u8], depth: u8) -> Result<T, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(DecodeError);
    }
    let mut msg = T::default();
    let mut r = Reader { data, pos: 0 };
    while r.pos < data.len() {
        let tag = r.varint()?;
        let number = tag >> 3;
        if number == 0 || number > u64::from(u32::MAX) {
            return Err(DecodeError);
        }
        let value = match (tag & 7) as u8 {
            WT_VARINT => Value::Varint(r.varint()?),
            WT_I64 => {
                r.bytes(8)?;
                Value::Fixed
            }
            WT_LEN => {
                let len = r.varint()?;
                Value::Len(r.bytes(usize::try_from(len).map_err(|_| DecodeError)?)?)
            }
            WT_I32 => {
                r.bytes(4)?;
                Value::Fixed
            }
            _ => return Err(DecodeError),
        };
        msg.merge_field(Field { number: number as u32, value, depth })?;
    }
    Ok(msg)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.data.get(self.pos).ok_or(DecodeError)?;
            self.pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(DecodeError)
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(len).filter(|&end| end <= self.data.len()).ok_or(DecodeError)?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }
}

enum Value<'a> {
    Varint(u64),
    Len(&'a [u8]),
    Fixed,
}

pub struct Field<'a> {
    pub number: u32,
    value: Value<'a>,
    depth: u8,
}

impl<'a> Field<'a> {
    fn varint(&self) -> Result<u64, DecodeError> {
        match self.value {
            Value::Varint(v) => Ok(v),
            _ => Err(DecodeError),
        }
    }

    pub fn int32(&self) -> Result<i32, DecodeError> {
        self.varint().map(|v| v as i32)
    }

    pub fn int64(&self) -> Result<i64, DecodeError> {
        self.varint().map(|v| v as i64)
    }

    pub fn uint32(&self) -> Result<u32, DecodeError> {
        self.varint().map(|v| v as u32)
    }

    pub fn uint64(&self) -> Result<u64, DecodeError> {
        self.varint()
    }

    pub fn bool(&self) -> Result<bool, DecodeError> {
        self.varint().map(|v| v != 0)
    }

    pub fn bytes(&self) -> Result<&'a [u8], DecodeError> {
        match self.value {
            Value::Len(b) => Ok(b),
            _ => Err(DecodeError),
        }
    }

    pub fn message<T: Message<'a>>(&self) -> Result<T, DecodeError> {
        decode_at(self.bytes()?, self.depth + 1)
    }

    pub fn lazy<T: Message<'a>>(&self) -> Result<Lazy<'a, T>, DecodeError> {
        if self.depth >= MAX_DEPTH {
            return Err(DecodeError);
        }
        let wire = Wire {
            data: self.bytes()?,
            depth: self.depth + 1,
            #[cfg(test)]
            reencode: |data, depth| decode_at::<T>(data, depth).map(|m| m.encode_to_vec()),
        };
        Ok(Lazy { repr: LazyRepr::Wire(wire), _t: PhantomData })
    }

    /// Packed or one element at a time.
    pub fn repeated<T>(&self, out: &mut Vec<T>, f: fn(u64) -> T) -> Result<(), DecodeError> {
        match self.value {
            Value::Varint(v) => out.push(f(v)),
            Value::Len(data) => {
                let mut r = Reader { data, pos: 0 };
                while r.pos < data.len() {
                    out.push(f(r.varint()?));
                }
            }
            Value::Fixed => return Err(DecodeError),
        }
        Ok(())
    }
}

/// A message field kept encoded until asked for, so only the message types a
/// program reads or writes get their code linked in.
pub struct Lazy<'a, T> {
    repr: LazyRepr<'a>,
    _t: PhantomData<fn() -> T>,
}

#[derive(Clone, Copy)]
enum LazyRepr<'a> {
    Wire(Wire<'a>),
    Value(&'a dyn Encode),
}

#[derive(Clone, Copy)]
struct Wire<'a> {
    data: &'a [u8],
    depth: u8,
    #[cfg(test)]
    reencode: fn(&'a [u8], u8) -> Result<Vec<u8>, DecodeError>,
}

#[cfg(test)]
std::thread_local! {
    static LAZY_FAILED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// Tests encode a lazy field by decoding and re-encoding it, so a round trip
/// checks every message type; true when one of them failed to decode since
/// the last call.
#[cfg(test)]
pub fn take_lazy_failure() -> bool {
    LAZY_FAILED.with(|f| f.replace(false))
}

impl Wire<'_> {
    #[cfg(not(test))]
    fn bytes(&self) -> &[u8] {
        self.data
    }

    #[cfg(test)]
    fn bytes(&self) -> alloc::borrow::Cow<'_, [u8]> {
        match (self.reencode)(self.data, self.depth) {
            Ok(v) => alloc::borrow::Cow::Owned(v),
            Err(_) => {
                LAZY_FAILED.with(|f| f.set(true));
                alloc::borrow::Cow::Borrowed(self.data)
            }
        }
    }
}

impl<'a, T: Encode> Lazy<'a, T> {
    pub fn new(value: &'a T) -> Self {
        Self { repr: LazyRepr::Value(value), _t: PhantomData }
    }
}

impl<'a, T: Message<'a>> Lazy<'a, T> {
    /// Fails for one made with `new`, which only encodes.
    pub fn get(&self) -> Result<T, DecodeError> {
        match self.repr {
            LazyRepr::Wire(w) => decode_at(w.data, w.depth),
            LazyRepr::Value(_) => Err(DecodeError),
        }
    }
}

impl<T> Lazy<'_, T> {
    fn encoded_len(&self) -> usize {
        match self.repr {
            LazyRepr::Wire(w) => w.bytes().len(),
            LazyRepr::Value(v) => v.encoded_len(),
        }
    }
}

impl<T> Clone for Lazy<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Lazy<'_, T> {}

impl<T> fmt::Debug for Lazy<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.repr {
            LazyRepr::Wire(w) => write!(f, "Lazy({} bytes)", w.data.len()),
            LazyRepr::Value(_) => f.write_str("Lazy(value)"),
        }
    }
}

fn varint_len(v: u64) -> usize {
    (64 - (v | 1).leading_zeros() as usize).div_ceil(7)
}

fn tag_len(number: u32) -> usize {
    varint_len(u64::from(number) << 3)
}

pub fn len_varint(number: u32, v: u64) -> usize {
    tag_len(number) + varint_len(v)
}

pub fn len_bytes(number: u32, len: usize) -> usize {
    tag_len(number) + varint_len(len as u64) + len
}

pub fn len_message(number: u32, m: &dyn Encode) -> usize {
    len_bytes(number, m.encoded_len())
}

pub fn len_lazy<T>(number: u32, m: &Lazy<'_, T>) -> usize {
    len_bytes(number, m.encoded_len())
}

pub fn len_packed<T: Copy>(number: u32, values: &[T], f: fn(T) -> u64) -> usize {
    if values.is_empty() { 0 } else { len_bytes(number, packed_len(values, f)) }
}

fn packed_len<T: Copy>(values: &[T], f: fn(T) -> u64) -> usize {
    values.iter().map(|&v| varint_len(f(v))).sum()
}

pub struct Writer<'b> {
    buf: &'b mut [u8],
    pos: usize,
}

impl Writer<'_> {
    fn raw_varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf[self.pos] = v as u8 | 0x80;
            self.pos += 1;
            v >>= 7;
        }
        self.buf[self.pos] = v as u8;
        self.pos += 1;
    }

    fn tag(&mut self, number: u32, wt: u8) {
        self.raw_varint(u64::from(number) << 3 | u64::from(wt));
    }

    pub fn varint(&mut self, number: u32, v: u64) {
        self.tag(number, WT_VARINT);
        self.raw_varint(v);
    }

    pub fn bytes(&mut self, number: u32, data: &[u8]) {
        self.tag(number, WT_LEN);
        self.raw_varint(data.len() as u64);
        self.buf[self.pos..self.pos + data.len()].copy_from_slice(data);
        self.pos += data.len();
    }

    pub fn message(&mut self, number: u32, m: &dyn Encode) {
        self.tag(number, WT_LEN);
        self.raw_varint(m.encoded_len() as u64);
        m.encode_fields(self);
    }

    pub fn lazy<T>(&mut self, number: u32, m: &Lazy<'_, T>) {
        match m.repr {
            LazyRepr::Wire(w) => self.bytes(number, &w.bytes()),
            LazyRepr::Value(v) => self.message(number, v),
        }
    }

    pub fn packed<T: Copy>(&mut self, number: u32, values: &[T], f: fn(T) -> u64) {
        if values.is_empty() {
            return;
        }
        self.tag(number, WT_LEN);
        self.raw_varint(packed_len(values, f) as u64);
        for &v in values {
            self.raw_varint(f(v));
        }
    }
}
