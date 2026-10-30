/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::Zeroable;

use crate::bits::Input;
use crate::{Error, Result, Source};

pub const WINDOW_BYTES: usize = 32768;
pub type Window = [u8; WINDOW_BYTES];

const WINDOW_MASK: usize = WINDOW_BYTES - 1;
const FAST_BITS: u32 = 9;
const FAST_SIZE: usize = 1 << FAST_BITS;

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
}

/// Streaming DEFLATE inside a zlib wrapper. Zeroed memory works as an instance
/// that was not started, so it can go straight to the heap.
pub struct Inflate {
    lit: Huffman,
    dist: Huffman,
    bitbuf: u32,
    bitcnt: u32,
    wpos: u32,
    stored_rem: u32,
    copy_rem: u32,
    copy_dist: u32,
    padding: u32,
    started: bool,
    block_active: bool,
    block_final: bool,
    stored: bool,
    ended: bool,
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
            bitbuf: 0,
            bitcnt: 0,
            wpos: 0,
            stored_rem: 0,
            copy_rem: 0,
            copy_dist: 0,
            padding: 0,
            started: false,
            block_active: false,
            block_final: false,
            stored: false,
            ended: false,
            failed: None,
        }
    }

    /// Reads the zlib header of a stream. Calling it again starts the next
    /// stream.
    pub fn start(&mut self, src: &mut dyn Source) -> Result<()> {
        self.bitbuf = 0;
        self.bitcnt = 0;
        self.wpos = 0;
        self.stored_rem = 0;
        self.copy_rem = 0;
        self.copy_dist = 0;
        self.padding = 0;
        self.started = false;
        self.block_active = false;
        self.block_final = false;
        self.stored = false;
        self.ended = false;
        self.failed = None;

        let mut input = Input::new(src, 0);
        let (Some(cmf), Some(flg)) = (input.byte()?, input.byte()?) else {
            return Err(Error::Truncated);
        };
        let (cmf, flg) = (u32::from(cmf), u32::from(flg));
        if cmf & 0x0F != 8 || ((cmf << 8) | flg) % 31 != 0 || flg & 0x20 != 0 {
            return Err(Error::Corrupt);
        }
        self.started = true;
        Ok(())
    }

    /// Fills `out`; fewer bytes than asked means the stream ended. `window` is
    /// the same buffer for the whole stream. An error stays until `start`.
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

    pub fn ended(&self) -> bool {
        self.ended
    }

    fn run(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<usize> {
        let mut produced = 0;
        while produced < out.len() && !self.ended {
            if !self.block_active {
                self.block_final = bits.take(1)? != 0;
                let kind = bits.take(2)?;
                if bits.input.eof() {
                    self.ended = true;
                    break;
                }
                match kind {
                    0 => {
                        bits.buf = 0;
                        bits.cnt = 0;
                        let (l0, l1) = (bits.input.byte()?, bits.input.byte()?);
                        bits.input.byte()?;
                        bits.input.byte()?;
                        let (Some(l0), Some(l1)) = (l0, l1) else {
                            self.ended = true;
                            break;
                        };
                        self.stored_rem = u32::from(l0) | (u32::from(l1) << 8);
                        self.stored = true;
                    }
                    1 => {
                        self.build_fixed();
                        self.stored = false;
                        self.copy_rem = 0;
                    }
                    2 => {
                        self.build_dynamic(bits)?;
                        self.stored = false;
                        self.copy_rem = 0;
                    }
                    _ => return Err(Error::Corrupt),
                }
                self.block_active = true;
            }

            produced += if self.stored {
                self.copy_stored(bits, window, &mut out[produced..])?
            } else {
                self.decode(bits, window, &mut out[produced..])?
            };
        }
        Ok(produced)
    }

    fn copy_stored(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<usize> {
        let mut produced = 0;
        let mut wpos = self.wpos as usize;
        while produced < out.len() && self.stored_rem > 0 {
            let Some(b) = bits.input.byte()? else {
                return Err(Error::Truncated);
            };
            out[produced] = b;
            produced += 1;
            window[wpos] = b;
            wpos = (wpos + 1) & WINDOW_MASK;
            self.stored_rem -= 1;
        }
        self.wpos = wpos as u32;
        if self.stored_rem == 0 {
            self.block_active = false;
            self.ended = self.block_final;
        }
        Ok(produced)
    }

    fn decode(&mut self, bits: &mut Bits<'_>, window: &mut Window, out: &mut [u8]) -> Result<usize> {
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
            if produced == n {
                break 'decode Ok(());
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
                self.block_active = false;
                self.ended = self.block_final;
                break 'decode Ok(());
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
        result.map(|()| produced)
    }

    fn build_fixed(&mut self) {
        let mut lengths = [8u8; 288];
        lengths[144..256].fill(9);
        lengths[256..280].fill(7);
        self.lit.construct(&lengths);
        self.dist.construct(&[5; 30]);
    }

    fn build_dynamic(&mut self, bits: &mut Bits<'_>) -> Result<()> {
        let hlit = bits.take(5)? as usize + 257;
        let hdist = bits.take(5)? as usize + 1;
        let hclen = bits.take(4)? as usize + 4;
        if hlit > 286 || hdist > 30 {
            return Err(Error::Corrupt);
        }

        let mut cl_lengths = [0u8; 19];
        for &order in &CL_ORDER[..hclen] {
            cl_lengths[usize::from(order)] = bits.take(3)? as u8;
        }
        // The distance table is rebuilt below, and a code-length table of its
        // own would cost 1.6 KB of stack.
        if self.dist.construct(&cl_lengths) < 0 {
            return Err(Error::Corrupt);
        }

        let mut lengths = [0u8; 286 + 30];
        let n = hlit + hdist;
        let mut idx = 0;
        while idx < n {
            let sym = bits.symbol(&self.dist)?;
            let (value, rep) = match sym {
                0..=15 => (sym as u8, 1),
                16 if idx == 0 => return Err(Error::Corrupt),
                16 => (lengths[idx - 1], 3 + bits.take(2)? as usize),
                17 => (0, 3 + bits.take(3)? as usize),
                _ => (0, 11 + bits.take(7)? as usize),
            };
            let end = (idx + rep).min(n);
            lengths[idx..end].fill(value);
            idx = end;
        }
        if self.lit.construct(&lengths[..hlit]) < 0 || self.dist.construct(&lengths[hlit..n]) < 0 {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}
