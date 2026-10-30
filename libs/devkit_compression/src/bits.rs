/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

use crate::{Error, Result, Source};

// Zero bits read past the end are fine as lookahead, which a final symbol
// needs; more than this means the stream was cut short.
const MAX_PADDING: u32 = 4;

/// The input of one `read` call: the source's current block as raw pointers,
/// because the block borrows from the source that the refill path needs back.
pub(crate) struct Input<'a> {
    src: &'a mut dyn Source,
    start: *const u8,
    ptr: *const u8,
    end: *const u8,
    padding: u32,
}

impl<'a> Input<'a> {
    pub(crate) fn new(src: &'a mut dyn Source, padding: u32) -> Self {
        Self { src, start: ptr::null(), ptr: ptr::null(), end: ptr::null(), padding }
    }

    #[inline(always)]
    pub(crate) fn byte(&mut self) -> Result<Option<u8>> {
        if self.ptr == self.end {
            return self.refill();
        }
        let b = unsafe { *self.ptr };
        self.ptr = unsafe { self.ptr.add(1) };
        Ok(Some(b))
    }

    /// `byte`, with zeros past the end of the input.
    #[inline(always)]
    pub(crate) fn byte_or_zero(&mut self) -> Result<u32> {
        match self.byte()? {
            Some(b) => Ok(u32::from(b)),
            None => self.pad(),
        }
    }

    pub(crate) fn eof(&self) -> bool {
        self.padding != 0
    }

    pub(crate) fn padding(&self) -> u32 {
        self.padding
    }

    #[cold]
    fn pad(&mut self) -> Result<u32> {
        self.padding += 1;
        if self.padding > MAX_PADDING { Err(Error::Truncated) } else { Ok(0) }
    }

    #[cold]
    #[inline(never)]
    fn refill(&mut self) -> Result<Option<u8>> {
        self.release();
        let block = self.src.fill_buf()?;
        let Some(&first) = block.first() else {
            return Ok(None);
        };
        self.start = block.as_ptr();
        self.end = self.start.wrapping_add(block.len());
        self.ptr = self.start.wrapping_add(1);
        Ok(Some(first))
    }

    fn release(&mut self) {
        let used = self.ptr as usize - self.start as usize;
        if used != 0 {
            self.src.consume(used);
        }
        self.start = self.ptr;
    }
}

impl Drop for Input<'_> {
    fn drop(&mut self) {
        self.release();
    }
}
