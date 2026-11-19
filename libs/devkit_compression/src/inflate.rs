/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_core::{Crc32, Zeroable};

use crate::adler32::Adler32;
use crate::bits::Input;
use crate::{Error, Result, Source};

pub const WINDOW_BYTES: usize = 32768;
pub type Window = [u8; WINDOW_BYTES];

const WINDOW_MASK: usize = WINDOW_BYTES - 1;
const FAST_BITS: u32 = 9;
const FAST_SIZE: usize = 1 << FAST_BITS;
// A length and distance pair takes at most 48 bits.
const SYMBOL_BYTES: usize = 8;
const MAX_LENGTHS: usize = 286 + 30;

const GZIP_FHCRC: u8 = 0x02;
const GZIP_FEXTRA: u8 = 0x04;
const GZIP_FNAME: u8 = 0x08;
const GZIP_FCOMMENT: u8 = 0x10;
const GZIP_RESERVED: u8 = 0xE0;

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CL_ORDER: [u8; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

#[derive(Clone)]
struct Huffman {
    count: [i16; 16],
    symbol: [i16; 288],
    fast: [u16; FAST_SIZE],
}

impl Huffman {
    const fn new() -> Self {
        Self { count: [0; 16], symbol: [0; 288], fast: [0; FAST_SIZE] }
    }

    /// Negative for an over-subscribed code, positive for an incomplete one.
    fn construct(&mut self, lengths: &[u8]) -> i32 {
        self.count = [0; 16];
        for &len in lengths {
            self.count[usize::from(len)] += 1;
        }
        self.fast = [0; FAST_SIZE];
        if usize::try_from(self.count[0]) == Ok(lengths.len()) {
            return 0;
        }

        let mut left = 1i32;
        for len in 1..=15 {
            left <<= 1;
            left -= i32::from(self.count[len]);
            if left < 0 {
                return left;
            }
        }

        let mut offs = [0i16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + self.count[len];
        }
        for (s, &len) in lengths.iter().enumerate() {
            if len != 0 {
                let off = &mut offs[usize::from(len)];
                self.symbol[*off as usize] = s as i16;
                *off += 1;
            }
        }

        let mut next_code = [0u16; 16];
        let mut code = 0u32;
        for len in 1..=15 {
            next_code[len] = code as u16;
            code = (code + self.count[len] as u32) << 1;
        }
        for (s, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let c = next_code[usize::from(len)];
            next_code[usize::from(len)] += 1;
            if u32::from(len) <= FAST_BITS {
                let rev = c.reverse_bits() >> (16 - len);
                for j in (usize::from(rev)..FAST_SIZE).step_by(1 << len) {
                    self.fast[j] = (u16::from(len) << 9) | s as u16;
                }
            }
        }
        left
    }
}

/// The bit buffer, kept out of `Inflate` for the time of a `read` so the
/// decode loop holds it in registers.
struct Bits<'a> {
    input: Input<'a>,
    buf: u32,
    cnt: u32,
}

impl Bits<'_> {
    #[inline(always)]
    fn take(&mut self, need: u32) -> Result<u32> {
        while self.cnt < need {
            self.buf |= self.input.byte_or_zero()? << self.cnt;
            self.cnt += 8;
        }
        let v = self.buf & ((1 << need) - 1);
        self.buf >>= need;
        self.cnt -= need;
        Ok(v)
    }

    #[inline(always)]
    fn symbol(&mut self, h: &Huffman) -> Result<u32> {
        while self.cnt < FAST_BITS {
            self.buf |= self.input.byte_or_zero()? << self.cnt;
            self.cnt += 8;
        }
        let e = u32::from(h.fast[self.buf as usize & (FAST_SIZE - 1)]);
        if e == 0 {
            return self.symbol_slow(h);
        }
        let len = e >> 9;
        self.buf >>= len;
        self.cnt -= len;
        Ok(e & 0x1FF)
    }

    #[cold]
    #[inline(never)]
    fn symbol_slow(&mut self, h: &Huffman) -> Result<u32> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=15 {
            code |= self.take(1)? as i32;
            let count = i32::from(h.count[len]);
            if code - first < count {
                return Ok(h.symbol[(index + code - first) as usize] as u32);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::Corrupt)
    }

    /// Holds at least `need` bits, with zeros past the end of the input;
    /// `false` while a pending source has nothing more yet.
    fn fill(&mut self, need: u32) -> Result<bool> {
        while self.cnt < need {
            let b = match self.input.byte()? {
                Some(b) => u32::from(b),
                None if self.input.more() => return Ok(false),
                None => self.input.pad()?,
            };
            self.buf |= b << self.cnt;
            self.cnt += 8;
        }
        Ok(true)
    }

    fn skip(&mut self, n: u32) {
        self.buf >>= n;
        self.cnt -= n;
    }

    fn align(&mut self) {
        self.skip(self.cnt % 8);
    }

    /// The next symbol and its length, left in the buffer; `None` while a
    /// pending source has too few bits for it yet.
    fn peek(&mut self, h: &Huffman) -> Result<Option<(u32, u32)>> {
        let full = self.fill(15)?;
        let e = u32::from(h.fast[self.buf as usize & (FAST_SIZE - 1)]);
        if e != 0 && e >> 9 <= self.cnt {
            return Ok(Some((e & 0x1FF, e >> 9)));
        }
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=15 {
            if len > self.cnt {
                return if full { Err(Error::Corrupt) } else { Ok(None) };
            }
            code |= ((self.buf >> (len - 1)) & 1) as i32;
            let count = i32::from(h.count[len as usize]);
            if code - first < count {
                return Ok(Some((h.symbol[(index + code - first) as usize] as u32, len)));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::Corrupt)
    }

    /// The next byte of a byte-aligned stream: the whole bytes the buffer
    /// holds, then the input. `None` while a pending source has nothing more yet.
    fn byte(&mut self) -> Result<Option<u8>> {
        debug_assert_eq!(self.cnt % 8, 0);
        if self.cnt >= 8 {
            // Zeros padded past the end of the input are the last bytes held.
            if self.cnt / 8 <= self.input.padding() {
                return Err(Error::Truncated);
            }
            let b = self.buf as u8;
            self.skip(8);
            return Ok(Some(b));
        }
        match self.input.byte()? {
            Some(b) => Ok(Some(b)),
            None if self.input.more() => Ok(None),
            None => Err(Error::Truncated),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Wrap {
    Raw,
    Zlib,
    Gzip,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Phase {
    Header,
    Block,
    Table,
    StoredLen,
    Stored,
    Codes,
    Trailer,
    Done,
}

/// Streaming DEFLATE, bare or inside a zlib or gzip wrapper whose checksum is
/// checked at the end. Zeroed memory works as an instance that was not
/// started, so it can go straight to the heap.
pub struct Inflate {
    lit: Huffman,
    dist: Huffman,
    lengths: [u8; MAX_LENGTHS],
    held: [u8; 8],
    bitbuf: u32,
    bitcnt: u32,
    wpos: u32,
    stored_rem: u32,
    copy_rem: u32,
    copy_dist: u32,
    padding: u32,
    total: u32,
    adler: Adler32,
    crc: Crc32,
    header_crc: Crc32,
    step: u16,
    count: u16,
    part: u16,
    hlit: u16,
    hdist: u16,
    hclen: u16,
    flags: u8,
    wrap: Wrap,
    phase: Phase,
    started: bool,
    block_final: bool,
    failed: Option<Error>,
}

unsafe impl Zeroable for Inflate {}

impl Default for Inflate {
    fn default() -> Self {
        Self::new()
    }
}

impl Inflate {
    pub const fn new() -> Self {
        Self {
            lit: Huffman::new(),
            dist: Huffman::new(),
            lengths: [0; MAX_LENGTHS],
            held: [0; 8],
            bitbuf: 0,
            bitcnt: 0,
            wpos: 0,
            stored_rem: 0,
            copy_rem: 0,
            copy_dist: 0,
            padding: 0,
            total: 0,
            adler: Adler32::new(),
            crc: Crc32::new(),
            header_crc: Crc32::new(),
            step: 0,
            count: 0,
            part: 0,
            hlit: 0,
            hdist: 0,
            hclen: 0,
            flags: 0,
            wrap: Wrap::Raw,
            phase: Phase::Header,
            started: false,
            block_final: false,
            failed: None,
        }
    }

    /// Reads the zlib header of a stream. Calling it again starts the next
    /// stream.
    pub fn start(&mut self, src: &mut dyn Source) -> Result<()> {
        self.begin(Wrap::Zlib, src)
    }

    /// Reads the gzip header of a stream: one member, whose CRC-32 and length
    /// are checked at its end.
    pub fn start_gzip(&mut self, src: &mut dyn Source) -> Result<()> {
        self.begin(Wrap::Gzip, src)
    }

    /// Starts a bare DEFLATE stream, one without a header or a checksum.
    pub fn start_raw(&mut self) {
        self.reset(Wrap::Raw);
        self.started = true;
    }

    fn begin(&mut self, wrap: Wrap, src: &mut dyn Source) -> Result<()> {
        self.reset(wrap);
        let mut bits = Bits { input: Input::new(src, 0), buf: 0, cnt: 0 };
        let result = self.header(&mut bits);
        self.bitbuf = bits.buf;
        self.bitcnt = bits.cnt;
        self.padding = bits.input.padding();
        result?;
        self.started = true;
        Ok(())
    }

    fn reset(&mut self, wrap: Wrap) {
        self.bitbuf = 0;
        self.bitcnt = 0;
        self.wpos = 0;
        self.stored_rem = 0;
        self.copy_rem = 0;
        self.copy_dist = 0;
        self.padding = 0;
        self.total = 0;
        self.adler = Adler32::new();
        self.crc = Crc32::new();
        self.header_crc = Crc32::new();
        self.step = 0;
        self.count = 0;
        self.flags = 0;
        self.wrap = wrap;
        self.phase = if wrap == Wrap::Raw { Phase::Block } else { Phase::Header };
        self.started = false;
        self.block_final = false;
        self.failed = None;
    }

    /// Fills `out`. Fewer bytes than asked means the stream ended or, with a
    /// pending source, that its input ran out for now; `ended` tells which.
    /// `window` is the same buffer for the whole stream. An error, a checksum
    /// that does not match included, stays until the next start.
    pub fn read(&mut self, src: &mut dyn Source, window: &mut Window, out: &mut [u8]) -> Result<usize> {
        if !self.started {
            return Err(Error::NotStarted);
        }
        if let Some(e) = self.failed {
            return Err(e);
        }
        let mut bits = Bits { input: Input::new(src, self.padding), buf: self.bitbuf, cnt: self.bitcnt };
        let result = self.run(&mut bits, window, out);
        self.bitbuf = bits.buf;
        self.bitcnt = bits.cnt;
        self.padding = bits.input.padding();
        if let Err(e) = result {
            self.failed = Some(e);
        }
        result
    }

    /// The last block was decoded and the checksum, if any, matched.
    pub fn ended(&self) -> bool {
        self.phase == Phase::Done
    }

    fn run(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<usize> {
        let pending = bits.input.more();
        let mut produced = 0;
        let mut summed = 0;
        let result = loop {
            let step = match self.phase {
                Phase::Header => self.header(bits),
                Phase::Block => self.block(bits),
                Phase::Table => self.table(bits),
                Phase::StoredLen => self.stored_len(bits),
                Phase::Stored | Phase::Codes if produced == out.len() => break Ok(()),
                Phase::Stored => self.copy_stored(bits, window, &mut out[produced..]).map(|(n, more)| {
                    produced += n;
                    more
                }),
                Phase::Codes => self.codes(bits, window, &mut out[produced..], pending).map(|(n, more)| {
                    produced += n;
                    more
                }),
                Phase::Trailer => {
                    self.sum(&out[summed..produced]);
                    summed = produced;
                    self.trailer(bits)
                }
                Phase::Done => break Ok(()),
            };
            match step {
                Ok(true) => {}
                Ok(false) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        self.sum(&out[summed..produced]);
        result.map(|()| produced)
    }

    fn sum(&mut self, data: &[u8]) {
        match self.wrap {
            Wrap::Raw => {}
            Wrap::Zlib => self.adler.update(data),
            Wrap::Gzip => {
                self.crc.update(data);
                self.total = self.total.wrapping_add(data.len() as u32);
            }
        }
    }

    fn header(&mut self, bits: &mut Bits<'_>) -> Result<bool> {
        match self.wrap {
            Wrap::Raw => {}
            Wrap::Zlib => {
                while self.step < 2 {
                    let Some(b) = bits.byte()? else { return Ok(false) };
                    self.held[usize::from(self.step)] = b;
                    self.step += 1;
                }
                let (cmf, flg) = (u32::from(self.held[0]), u32::from(self.held[1]));
                if cmf & 0x0F != 8 || ((cmf << 8) | flg) % 31 != 0 || flg & 0x20 != 0 {
                    return Err(Error::Corrupt);
                }
            }
            Wrap::Gzip => {
                if !self.gzip_header(bits)? {
                    return Ok(false);
                }
            }
        }
        self.step = 0;
        self.phase = Phase::Block;
        Ok(true)
    }

    /// Steps 0-9 are the fixed fields, 10-12 FEXTRA, 13 FNAME, 14 FCOMMENT and
    /// 15 FHCRC.
    fn gzip_header(&mut self, bits: &mut Bits<'_>) -> Result<bool> {
        while self.step < 16 {
            let present = match self.step {
                10..=12 => self.flags & GZIP_FEXTRA != 0,
                13 => self.flags & GZIP_FNAME != 0,
                14 => self.flags & GZIP_FCOMMENT != 0,
                15 => self.flags & GZIP_FHCRC != 0,
                _ => true,
            };
            if !present || (self.step == 12 && self.count == 0) {
                self.step += 1;
                continue;
            }
            let Some(b) = bits.byte()? else { return Ok(false) };
            if self.step == 15 {
                self.held[usize::from(self.count)] = b;
                self.count += 1;
                if self.count == 2 {
                    if u16::from_le_bytes([self.held[0], self.held[1]]) != self.header_crc.value() as u16 {
                        return Err(Error::Corrupt);
                    }
                    self.step += 1;
                }
                continue;
            }
            self.header_crc.update(&[b]);
            match self.step {
                0 if b != 0x1F => return Err(Error::Corrupt),
                1 if b != 0x8B => return Err(Error::Corrupt),
                2 if b != 8 => return Err(Error::Corrupt),
                3 if b & GZIP_RESERVED != 0 => return Err(Error::Corrupt),
                3 => self.flags = b,
                10 => self.count = u16::from(b),
                11 => self.count |= u16::from(b) << 8,
                12 => {
                    self.count -= 1;
                    continue;
                }
                13 | 14 if b != 0 => continue,
                _ => {}
            }
            self.step += 1;
        }
        Ok(true)
    }

    fn block(&mut self, bits: &mut Bits<'_>) -> Result<bool> {
        if !bits.fill(3)? {
            return Ok(false);
        }
        if bits.input.eof() {
            if self.wrap != Wrap::Raw {
                return Err(Error::Truncated);
            }
            self.phase = Phase::Done;
            return Ok(true);
        }
        self.block_final = bits.take(1)? != 0;
        self.step = 0;
        self.phase = match bits.take(2)? {
            0 => {
                bits.align();
                Phase::StoredLen
            }
            1 => {
                self.build_fixed();
                Phase::Codes
            }
            2 => Phase::Table,
            _ => return Err(Error::Corrupt),
        };
        Ok(true)
    }

    fn end_block(&mut self, bits: &mut Bits<'_>) {
        self.phase = if !self.block_final {
            Phase::Block
        } else if self.wrap == Wrap::Raw {
            Phase::Done
        } else {
            bits.align();
            self.step = 0;
            Phase::Trailer
        };
    }

    fn stored_len(&mut self, bits: &mut Bits<'_>) -> Result<bool> {
        while self.step < 4 {
            let Some(b) = bits.byte()? else { return Ok(false) };
            self.held[usize::from(self.step)] = b;
            self.step += 1;
        }
        let len = u16::from_le_bytes([self.held[0], self.held[1]]);
        if len != !u16::from_le_bytes([self.held[2], self.held[3]]) {
            return Err(Error::Corrupt);
        }
        self.stored_rem = u32::from(len);
        self.phase = Phase::Stored;
        if len == 0 {
            self.end_block(bits);
        }
        Ok(true)
    }

    fn copy_stored(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<(usize, bool)> {
        let mut produced = 0;
        let mut wpos = self.wpos as usize;
        let mut more = true;
        while produced < out.len() && self.stored_rem > 0 {
            let Some(b) = bits.byte()? else {
                more = false;
                break;
            };
            out[produced] = b;
            produced += 1;
            window[wpos] = b;
            wpos = (wpos + 1) & WINDOW_MASK;
            self.stored_rem -= 1;
        }
        self.wpos = wpos as u32;
        if self.stored_rem == 0 {
            self.end_block(bits);
        }
        Ok((produced, more))
    }

    fn trailer(&mut self, bits: &mut Bits<'_>) -> Result<bool> {
        let need = if self.wrap == Wrap::Gzip { 8 } else { 4 };
        while usize::from(self.step) < need {
            let Some(b) = bits.byte()? else { return Ok(false) };
            self.held[usize::from(self.step)] = b;
            self.step += 1;
        }
        let h = &self.held;
        let ok = if self.wrap == Wrap::Gzip {
            u32::from_le_bytes([h[0], h[1], h[2], h[3]]) == self.crc.value()
                && u32::from_le_bytes([h[4], h[5], h[6], h[7]]) == self.total
        } else {
            u32::from_be_bytes([h[0], h[1], h[2], h[3]]) == self.adler.value()
        };
        if !ok {
            return Err(Error::Corrupt);
        }
        self.phase = Phase::Done;
        Ok(true)
    }

    fn codes(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8], pending: bool) -> Result<(usize, bool)> {
        let mut produced = 0;
        while produced < out.len() && self.phase == Phase::Codes {
            if self.copy_rem != 0 {
                produced += self.copy(window, &mut out[produced..]);
            } else if self.step == 0 && !pending {
                produced += self.decode::<false>(bits, window, &mut out[produced..])?;
            } else if self.step == 0 && bits.input.left() >= SYMBOL_BYTES {
                produced += self.decode::<true>(bits, window, &mut out[produced..])?;
            } else {
                match self.symbol_step(bits, window, &mut out[produced..])? {
                    Some(n) => produced += n,
                    None => return Ok((produced, false)),
                }
            }
        }
        Ok((produced, true))
    }

    fn copy(&mut self, window: &mut Window, out: &mut [u8]) -> usize {
        let run = (self.copy_rem as usize).min(out.len());
        let mut wpos = self.wpos as usize;
        let dist = self.copy_dist as usize;
        for slot in &mut out[..run] {
            let b = window[wpos.wrapping_sub(dist) & WINDOW_MASK];
            *slot = b;
            window[wpos] = b;
            wpos = (wpos + 1) & WINDOW_MASK;
        }
        self.wpos = wpos as u32;
        self.copy_rem -= run as u32;
        run
    }

    /// One symbol with its extra bits, or the part of a length and distance
    /// pair the input holds; nothing is taken from a part that is cut short.
    fn symbol_step(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<Option<usize>> {
        if self.step == 0 {
            let Some((sym, len)) = bits.peek(&self.lit)? else { return Ok(None) };
            if sym < 256 {
                bits.skip(len);
                out[0] = sym as u8;
                window[self.wpos as usize] = sym as u8;
                self.wpos = (self.wpos + 1) & WINDOW_MASK as u32;
                return Ok(Some(1));
            }
            if sym == 256 {
                bits.skip(len);
                self.end_block(bits);
                return Ok(Some(0));
            }
            let sym = (sym - 257) as usize;
            if sym >= LEN_BASE.len() {
                return Err(Error::Corrupt);
            }
            let extra = u32::from(LEN_EXTRA[sym]);
            if !bits.fill(len + extra)? {
                return Ok(None);
            }
            bits.skip(len);
            self.part = LEN_BASE[sym] + bits.take(extra)? as u16;
            self.step = 1;
        }
        if self.step == 1 {
            let Some((ds, len)) = bits.peek(&self.dist)? else { return Ok(None) };
            if ds as usize >= DIST_BASE.len() {
                return Err(Error::Corrupt);
            }
            bits.skip(len);
            self.count = ds as u16;
            self.step = 2;
        }
        let ds = usize::from(self.count);
        let extra = u32::from(DIST_EXTRA[ds]);
        if !bits.fill(extra)? {
            return Ok(None);
        }
        self.copy_dist = u32::from(DIST_BASE[ds]) + bits.take(extra)?;
        self.copy_rem = u32::from(self.part);
        self.step = 0;
        Ok(Some(0))
    }

    /// With `PENDING`, stops before a symbol the current input block may not
    /// hold all of.
    fn decode<const PENDING: bool>(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<usize> {
        let n = out.len();
        let mut produced = 0;
        let mut wpos = self.wpos as usize;
        let mut copy_rem = self.copy_rem as usize;
        let mut copy_dist = self.copy_dist as usize;
        let result = 'decode: loop {
            if copy_rem != 0 {
                let run = copy_rem.min(n - produced);
                for slot in &mut out[produced..produced + run] {
                    let b = window[wpos.wrapping_sub(copy_dist) & WINDOW_MASK];
                    *slot = b;
                    window[wpos] = b;
                    wpos = (wpos + 1) & WINDOW_MASK;
                }
                produced += run;
                copy_rem -= run;
            }
            if produced == n || (PENDING && bits.input.left() < SYMBOL_BYTES) {
                break 'decode Ok(false);
            }

            let sym = match bits.symbol(&self.lit) {
                Ok(sym) => sym as usize,
                Err(e) => break 'decode Err(e),
            };
            if sym < 256 {
                unsafe { *out.get_unchecked_mut(produced) = sym as u8 };
                produced += 1;
                window[wpos] = sym as u8;
                wpos = (wpos + 1) & WINDOW_MASK;
            } else if sym == 256 {
                break 'decode Ok(true);
            } else {
                let sym = sym - 257;
                if sym >= LEN_BASE.len() {
                    break 'decode Err(Error::Corrupt);
                }
                let dist = bits.take(u32::from(LEN_EXTRA[sym])).and_then(|extra| {
                    let ds = bits.symbol(&self.dist)? as usize;
                    if ds >= DIST_BASE.len() {
                        return Err(Error::Corrupt);
                    }
                    let d = usize::from(DIST_BASE[ds]) + bits.take(u32::from(DIST_EXTRA[ds]))? as usize;
                    Ok((usize::from(LEN_BASE[sym]) + extra as usize, d))
                });
                match dist {
                    Ok((len, d)) => (copy_rem, copy_dist) = (len, d),
                    Err(e) => break 'decode Err(e),
                }
            }
        };
        self.wpos = wpos as u32;
        self.copy_rem = copy_rem as u32;
        self.copy_dist = copy_dist as u32;
        if result? {
            self.end_block(bits);
        }
        Ok(produced)
    }

    fn build_fixed(&mut self) {
        let mut lengths = [8u8; 288];
        lengths[144..256].fill(9);
        lengths[256..280].fill(7);
        self.lit.construct(&lengths);
        self.dist.construct(&[5; 30]);
    }

    fn table(&mut self, bits: &mut Bits<'_>) -> Result<bool> {
        if self.step == 0 {
            if !bits.fill(14)? {
                return Ok(false);
            }
            self.hlit = bits.take(5)? as u16 + 257;
            self.hdist = bits.take(5)? as u16 + 1;
            self.hclen = bits.take(4)? as u16 + 4;
            if self.hlit > 286 || self.hdist > 30 {
                return Err(Error::Corrupt);
            }
            self.lengths[..19].fill(0);
            self.count = 0;
            self.step = 1;
        }
        if self.step == 1 {
            while self.count < self.hclen {
                if !bits.fill(3)? {
                    return Ok(false);
                }
                self.lengths[usize::from(CL_ORDER[usize::from(self.count)])] = bits.take(3)? as u8;
                self.count += 1;
            }
            // The distance table is rebuilt below, and a code-length table of
            // its own would cost another 1.6 KB.
            if self.dist.construct(&self.lengths[..19]) < 0 {
                return Err(Error::Corrupt);
            }
            self.count = 0;
            self.step = 2;
        }
        let n = usize::from(self.hlit + self.hdist);
        while usize::from(self.count) < n {
            let Some((sym, len)) = bits.peek(&self.dist)? else { return Ok(false) };
            let extra = match sym {
                16 => 2,
                17 => 3,
                18 => 7,
                _ => 0,
            };
            if !bits.fill(len + extra)? {
                return Ok(false);
            }
            bits.skip(len);
            let idx = usize::from(self.count);
            let (value, rep) = match sym {
                0..=15 => (sym as u8, 1),
                16 if idx == 0 => return Err(Error::Corrupt),
                16 => (self.lengths[idx - 1], 3 + bits.take(2)? as usize),
                17 => (0, 3 + bits.take(3)? as usize),
                _ => (0, 11 + bits.take(7)? as usize),
            };
            let end = (idx + rep).min(n);
            self.lengths[idx..end].fill(value);
            self.count = end as u16;
        }
        let hlit = usize::from(self.hlit);
        if self.lit.construct(&self.lengths[..hlit]) < 0 || self.dist.construct(&self.lengths[hlit..n]) < 0 {
            return Err(Error::Corrupt);
        }
        self.step = 0;
        self.copy_rem = 0;
        self.phase = Phase::Codes;
        Ok(true)
    }
}
