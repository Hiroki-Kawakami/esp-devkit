/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::Zeroable;

use crate::bits::Input;
use crate::{Error, Result, Source};

const MAX_CODES: usize = 4096;
const NO_CODE: u32 = 0xFFFF;

/// The two variants in use: GIF packs codes LSB-first and widens a code when
/// the next free entry reaches `1 << width`; TIFF packs them MSB-first with
/// `literal_bits` 8 and widens one entry early.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LzwOptions {
    pub literal_bits: u8,
    pub msb_first: bool,
    pub early_change: bool,
}

/// The tables, 16 KB the caller places.
#[repr(C)]
pub struct LzwWork {
    prefix: [u16; MAX_CODES],
    suffix: [u8; MAX_CODES],
    stack: [u8; MAX_CODES],
}

unsafe impl Zeroable for LzwWork {}

/// Streaming variable-width LZW, 12-bit codes at most. Running out of input
/// ends a stream as an end-of-information code would.
#[derive(Default)]
pub struct Lzw {
    bitbuf: u32,
    bitcnt: u32,
    stack_len: usize,
    clear: u32,
    next: u32,
    prev: u32,
    prev_first: u8,
    width: u32,
    literal_bits: u8,
    msb_first: bool,
    early_change: bool,
    started: bool,
    ended: bool,
    failed: Option<Error>,
}

unsafe impl Zeroable for Lzw {}

impl Lzw {
    pub const fn new() -> Self {
        Self {
            bitbuf: 0,
            bitcnt: 0,
            stack_len: 0,
            clear: 0,
            next: 0,
            prev: 0,
            prev_first: 0,
            width: 0,
            literal_bits: 0,
            msb_first: false,
            early_change: false,
            started: false,
            ended: false,
            failed: None,
        }
    }

    /// Calling it again starts the next stream with the same work area.
    /// `literal_bits` outside 2..=8 fails.
    pub fn start(&mut self, options: &LzwOptions, work: &mut LzwWork) -> Result<()> {
        *self = Self::new();
        if !(2..=8).contains(&options.literal_bits) {
            return Err(Error::Corrupt);
        }
        self.literal_bits = options.literal_bits;
        self.msb_first = options.msb_first;
        self.early_change = options.early_change;
        self.clear = 1 << options.literal_bits;
        for (i, slot) in work.suffix[..self.clear as usize].iter_mut().enumerate() {
            *slot = i as u8;
        }
        self.restart_table();
        self.started = true;
        Ok(())
    }

    /// Fills `out`; fewer bytes than asked means the stream ended. An error
    /// stays until `start`.
    pub fn read(&mut self, src: &mut dyn Source, work: &mut LzwWork, out: &mut [u8]) -> Result<usize> {
        if !self.started {
            return Err(Error::NotStarted);
        }
        if let Some(e) = self.failed {
            return Err(e);
        }
        let mut input = Input::new(src, 0);
        let result = self.run(&mut input, work, out);
        if let Err(e) = result {
            self.failed = Some(e);
        }
        result
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    fn restart_table(&mut self) {
        self.width = u32::from(self.literal_bits) + 1;
        self.next = self.clear + 2;
        self.prev = NO_CODE;
    }

    #[inline(always)]
    fn next_code(&mut self, input: &mut Input<'_>) -> Result<Option<u32>> {
        while self.bitcnt < self.width {
            let Some(c) = input.byte()? else {
                return Ok(None);
            };
            if self.msb_first {
                self.bitbuf = (self.bitbuf << 8) | u32::from(c);
            } else {
                self.bitbuf |= u32::from(c) << self.bitcnt;
            }
            self.bitcnt += 8;
        }
        let mask = (1 << self.width) - 1;
        self.bitcnt -= self.width;
        let code = if self.msb_first {
            let code = (self.bitbuf >> self.bitcnt) & mask;
            self.bitbuf &= (1 << self.bitcnt) - 1;
            code
        } else {
            let code = self.bitbuf & mask;
            self.bitbuf >>= self.width;
            code
        };
        Ok(Some(code))
    }

    fn run(&mut self, input: &mut Input<'_>, work: &mut LzwWork, out: &mut [u8]) -> Result<usize> {
        let mut produced = 0;
        loop {
            let take = self.stack_len.min(out.len() - produced);
            for slot in &mut out[produced..produced + take] {
                self.stack_len -= 1;
                *slot = work.stack[self.stack_len & (MAX_CODES - 1)];
            }
            produced += take;
            if produced == out.len() || self.ended {
                return Ok(produced);
            }

            let code = match self.next_code(input)? {
                Some(code) if code != self.clear + 1 => code,
                _ => {
                    self.ended = true;
                    return Ok(produced);
                }
            };
            if code == self.clear {
                self.restart_table();
                continue;
            }
            if self.prev == NO_CODE {
                if code > self.clear {
                    return Err(Error::Corrupt);
                }
                work.stack[0] = code as u8;
                self.stack_len = 1;
                self.prev = code;
                self.prev_first = code as u8;
                continue;
            }

            let mut len = 0;
            let mut c = if code < self.next {
                code
            } else if code == self.next && (self.next as usize) < MAX_CODES {
                work.stack[0] = self.prev_first;
                len = 1;
                self.prev
            } else {
                return Err(Error::Corrupt);
            };
            // Every entry's prefix is an older code, so a chain has fewer links
            // than the stack has bytes.
            while c > self.clear {
                unsafe {
                    *work.stack.get_unchecked_mut(len) = *work.suffix.get_unchecked(c as usize);
                    c = u32::from(*work.prefix.get_unchecked(c as usize));
                }
                len += 1;
            }
            let first = c as u8;
            work.stack[len] = first;
            self.stack_len = len + 1;

            if (self.next as usize) < MAX_CODES {
                work.prefix[self.next as usize] = self.prev as u16;
                work.suffix[self.next as usize] = first;
                self.next += 1;
                if self.width < 12 && self.next + u32::from(self.early_change) >= (1 << self.width) {
                    self.width += 1;
                }
            }
            self.prev = code;
            self.prev_first = first;
        }
    }
}
