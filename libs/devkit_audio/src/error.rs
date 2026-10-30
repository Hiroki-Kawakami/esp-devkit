/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    InvalidArg,
    InvalidSize,
    InvalidState,
    /// The bitstream breaks its format.
    InvalidData,
    InvalidCrc,
    NotSupported,
    NoMemory,
}

pub type Result<T> = core::result::Result<T, Error>;

impl From<idf::Error> for Error {
    fn from(_: idf::Error) -> Self {
        Error::NoMemory
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::InvalidArg => "invalid argument",
            Error::InvalidSize => "invalid size",
            Error::InvalidState => "invalid state",
            Error::InvalidData => "invalid data",
            Error::InvalidCrc => "CRC mismatch",
            Error::NotSupported => "not supported",
            Error::NoMemory => "out of memory",
        })
    }
}
