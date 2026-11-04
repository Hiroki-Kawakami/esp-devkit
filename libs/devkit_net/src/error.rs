/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    NotStarted,
    AlreadyStarted,
    Unsupported,
    AddrInUse,
    AddrNotAvailable,
    NoRoute,
    MessageTooLarge,
    NotConnected,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    BrokenPipe,
    TimedOut,
    NoMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::NotStarted => "network stack not started",
            Error::AlreadyStarted => "network stack already started",
            Error::Unsupported => "not supported",
            Error::AddrInUse => "address in use",
            Error::AddrNotAvailable => "address not available",
            Error::NoRoute => "no route to host",
            Error::MessageTooLarge => "message too large",
            Error::NotConnected => "not connected",
            Error::ConnectionRefused => "connection refused",
            Error::ConnectionReset => "connection reset",
            Error::ConnectionAborted => "connection aborted",
            Error::BrokenPipe => "broken pipe",
            Error::TimedOut => "timed out",
            Error::NoMemory => "out of memory",
        })
    }
}

pub type Result<T> = core::result::Result<T, Error>;
