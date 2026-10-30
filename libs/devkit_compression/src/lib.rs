/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

mod bits;
mod inflate;
mod lzw;

use core::fmt;

pub use inflate::{Inflate, WINDOW_BYTES, Window};
pub use lzw::{Lzw, LzwOptions, LzwWork};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    Corrupt,
    Truncated,
    /// The `Source` failed; what went wrong is the source's to report.
    Source,
    NotStarted,
}

pub type Result<T> = core::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Corrupt => "corrupt stream",
            Self::Truncated => "truncated stream",
            Self::Source => "source failed",
            Self::NotStarted => "stream not started",
        })
    }
}

/// Feeds a decompressor block by block, as `std::io::BufRead` does.
pub trait Source {
    /// The bytes not consumed yet, or the next block; empty at the end of the
    /// input.
    fn fill_buf(&mut self) -> Result<&[u8]>;
    fn consume(&mut self, n: usize);
}

impl Source for &[u8] {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(self)
    }

    fn consume(&mut self, n: usize) {
        *self = &self[n..];
    }
}

#[cfg(test)]
mod tests;
