/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

// Reads past `end` return zeros; `buf` has 8 readable bytes after `end`.
pub(crate) struct Bits<'a> {
    buf: &'a [u8],
    pub pos: u32,
    pub end: u32,
}

impl<'a> Bits<'a> {
    pub fn new(buf: &'a [u8], pos: u32, end: u32) -> Self {
        assert!(buf.len() >= end as usize / 8 + 8);
        Self { buf, pos, end }
    }

    #[inline(always)]
    pub fn show(&self) -> u32 {
        if self.pos >= self.end + 32 {
            return 0;
        }
        let i = (self.pos >> 3) as usize;
        let p = unsafe { self.buf.get_unchecked(i..i + 5) };
        let v = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
        let s = self.pos & 7;
        if s != 0 { v << s | u32::from(p[4]) >> (8 - s) } else { v }
    }

    #[inline(always)]
    pub fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.show() >> (32 - n);
        self.pos += n;
        v
    }

    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        self.get(1) != 0
    }

    pub fn overrun(&self) -> bool {
        self.pos > self.end
    }

    pub fn left(&self) -> u32 {
        self.end.saturating_sub(self.pos)
    }
}
