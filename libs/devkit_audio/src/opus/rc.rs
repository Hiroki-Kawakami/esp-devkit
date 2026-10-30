/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

const TOP: u32 = 1 << 31;
const BOT: u32 = 1 << 23;

#[inline(always)]
pub(super) fn ilog(v: u32) -> i32 {
    32 - v.leading_zeros() as i32
}

pub(super) struct Rc<'a> {
    buf: &'a [u8],
    pub storage: u32,
    end_offs: u32,
    end_window: u32,
    nend_bits: i32,
    pub nbits_total: i32,
    offs: u32,
    pub rng: u32,
    val: u32,
    ext: u32,
    rem: i32,
    pub error: bool,
}

impl<'a> Rc<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut rc = Self {
            buf,
            storage: buf.len() as u32,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: 9,
            offs: 0,
            rng: 128,
            val: 0,
            ext: 0,
            rem: 0,
            error: false,
        };
        rc.rem = rc.byte();
        rc.val = (127 - (rc.rem >> 1)) as u32;
        rc.normalize();
        rc
    }

    #[inline(always)]
    fn byte(&mut self) -> i32 {
        if self.offs < self.storage {
            let b = unsafe { *self.buf.get_unchecked(self.offs as usize) };
            self.offs += 1;
            i32::from(b)
        } else {
            0
        }
    }

    fn byte_from_end(&mut self) -> u32 {
        if self.end_offs < self.storage {
            self.end_offs += 1;
            u32::from(self.buf[(self.storage - self.end_offs) as usize])
        } else {
            0
        }
    }

    #[inline(always)]
    fn normalize(&mut self) {
        while self.rng <= BOT {
            self.nbits_total += 8;
            self.rng <<= 8;
            let prev = self.rem;
            self.rem = self.byte();
            let sym = (prev << 8 | self.rem) >> 1;
            self.val = ((self.val << 8).wrapping_add(255 & !sym as u32)) & (TOP - 1);
        }
    }

    pub fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft;
        let s = self.val / self.ext;
        ft - (s + 1).min(ft)
    }

    pub fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        let ft = 1u32 << bits;
        ft - (s + 1).min(ft)
    }

    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext.wrapping_mul(ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 { self.ext.wrapping_mul(fh - fl) } else { self.rng.wrapping_sub(s) };
        self.normalize();
    }

    pub fn bit_logp(&mut self, logp: u32) -> bool {
        let s = self.rng >> logp;
        let bit = self.val < s;
        if bit {
            self.rng = s;
        } else {
            self.val -= s;
            self.rng -= s;
        }
        self.normalize();
        bit
    }

    #[inline]
    pub fn icdf(&mut self, icdf: &[u8], ftb: u32) -> i32 {
        let r = self.rng >> ftb;
        let mut s = self.rng;
        let mut k = 0;
        let t = loop {
            let t = s;
            // Every table ends in 0, which ends the loop.
            s = r * u32::from(unsafe { *icdf.get_unchecked(k) });
            if self.val >= s {
                break t;
            }
            k += 1;
        };
        self.val -= s;
        self.rng = t - s;
        self.normalize();
        k as i32
    }

    pub fn bits(&mut self, bits: i32) -> u32 {
        let mut window = self.end_window;
        let mut available = self.nend_bits;
        if available < bits {
            loop {
                window |= self.byte_from_end() << available;
                available += 8;
                if available > 24 {
                    break;
                }
            }
        }
        let v = window & ((1u32 << bits) - 1);
        self.end_window = window >> bits;
        self.nend_bits = available - bits;
        self.nbits_total += bits;
        v
    }

    pub fn uint(&mut self, ft: u32) -> u32 {
        let top = ft - 1;
        let ftb = ilog(top);
        if ftb > 8 {
            let raw = ftb - 8;
            let fth = (top >> raw) + 1;
            let s = self.decode(fth);
            self.update(s, s + 1, fth);
            let t = s << raw | self.bits(raw);
            if t <= top {
                return t;
            }
            self.error = true;
            return top;
        }
        let s = self.decode(ft);
        self.update(s, s + 1, ft);
        s
    }

    pub fn tell_frac(&self) -> u32 {
        let mut lg = ilog(self.rng);
        let mut r = self.rng >> (lg - 16);
        for _ in 0..3 {
            r = r.wrapping_mul(r) >> 15;
            let b = (r >> 16) as i32;
            lg = lg << 1 | b;
            r >>= b;
        }
        ((self.nbits_total as u32) << 3).wrapping_sub(lg as u32)
    }

    #[inline(always)]
    pub fn tell(&self) -> i32 {
        self.nbits_total - ilog(self.rng)
    }
}
