/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Memory,
    Open,
    Arena,
    Format(&'static str),
    Interrupted,
    Io,
    NotSeekable,
}

pub type Result<T> = core::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Memory => "out of memory",
            Self::Open => "cannot open the file",
            Self::Arena => "unusable arena",
            Self::Format(text) => text,
            Self::Interrupted => "interrupted",
            Self::Io => "read failed",
            Self::NotSeekable => "cannot seek there",
        })
    }
}

impl core::error::Error for Error {}
