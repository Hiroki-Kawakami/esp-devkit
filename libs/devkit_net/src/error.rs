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
    HostUnreachable,
    MessageTooLarge,
    NotConnected,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    BrokenPipe,
    TimedOut,
    NoMemory,
    /// The link had no room for the frame; it was not sent.
    WouldBlock,
    InvalidInput,
    /// The name does not exist or has no address.
    NotFound,
    NoNameServer,
    /// The peer sent what the protocol does not allow.
    InvalidData,
    TooManyRedirects,
    Tls(devkit_crypto::Error),
    /// A file the data went to.
    Fs(idf::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Tls(e) => return write!(f, "TLS: {e}"),
            Error::Fs(e) => return write!(f, "file: {e}"),
            _ => {}
        }
        f.write_str(match self {
            Error::NotStarted => "network stack not started",
            Error::AlreadyStarted => "network stack already started",
            Error::Unsupported => "not supported",
            Error::AddrInUse => "address in use",
            Error::AddrNotAvailable => "address not available",
            Error::NoRoute => "no route to host",
            Error::HostUnreachable => "host unreachable",
            Error::MessageTooLarge => "message too large",
            Error::NotConnected => "not connected",
            Error::ConnectionRefused => "connection refused",
            Error::ConnectionReset => "connection reset",
            Error::ConnectionAborted => "connection aborted",
            Error::BrokenPipe => "broken pipe",
            Error::TimedOut => "timed out",
            Error::NoMemory => "out of memory",
            Error::WouldBlock => "operation would block",
            Error::InvalidInput => "invalid input",
            Error::NotFound => "name not found",
            Error::NoNameServer => "no name server",
            Error::InvalidData => "invalid data",
            Error::TooManyRedirects => "too many redirects",
            Error::Tls(_) | Error::Fs(_) => unreachable!(),
        })
    }
}

pub type Result<T> = core::result::Result<T, Error>;
