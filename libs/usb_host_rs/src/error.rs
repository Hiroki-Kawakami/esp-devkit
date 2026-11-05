/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    AlreadyStarted,
    Unsupported,
    NoDevice,
    /// The endpoint answered STALL; `Endpoint::clear_halt` resumes it.
    Stall,
    /// The device sent more than the transfer asked for.
    Overflow,
    Canceled,
    TimedOut,
    /// Another driver holds the interface.
    Busy,
    NotFound,
    InvalidArgument,
    InvalidResponse,
    NoMemory,
    Failed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::AlreadyStarted => "already started",
            Error::Unsupported => "not supported",
            Error::NoDevice => "no device",
            Error::Stall => "stall",
            Error::Overflow => "overflow",
            Error::Canceled => "canceled",
            Error::TimedOut => "timed out",
            Error::Busy => "busy",
            Error::NotFound => "not found",
            Error::InvalidArgument => "invalid argument",
            Error::InvalidResponse => "invalid response",
            Error::NoMemory => "out of memory",
            Error::Failed => "failed",
        })
    }
}

pub type Result<T> = core::result::Result<T, Error>;
