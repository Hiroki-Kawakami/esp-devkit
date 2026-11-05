/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    InvalidLength,
    InvalidKey,
    /// A signature, tag or MAC did not match.
    Verify,
    Unsupported,
    NoMemory,
    /// Malformed DER, certificate, bundle or TLS message.
    Decode,
    Certificate(CertError),
    /// The peer sent this fatal TLS alert.
    Alert(u8),
    /// The peer broke the TLS protocol; a fatal alert went out.
    Protocol,
    /// No cipher suite, group or signature scheme in common.
    HandshakeFailure,
    /// The TLS connection is closed.
    Closed,
    /// The CA bundle slot about to be written is still mapped.
    Busy,
    /// Flash access failed, with its `esp_err_t`.
    Flash(i32),
    /// Any other PSA failure, with its status.
    Psa(i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CertError {
    Expired,
    NotYetValid,
    UnknownIssuer,
    BadSignature,
    NameMismatch,
    NotCa,
    BadUsage,
    PathTooLong,
    NameConstraint,
    Distrusted,
    UnsupportedAlgorithm,
    UnknownCritical,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidLength => f.write_str("invalid length"),
            Error::InvalidKey => f.write_str("invalid key"),
            Error::Verify => f.write_str("verification failed"),
            Error::Unsupported => f.write_str("not supported"),
            Error::NoMemory => f.write_str("out of memory"),
            Error::Decode => f.write_str("malformed input"),
            Error::Certificate(e) => write!(f, "certificate: {e:?}"),
            Error::Alert(code) => write!(f, "TLS alert {code} from the peer"),
            Error::Protocol => f.write_str("TLS protocol violation"),
            Error::HandshakeFailure => f.write_str("TLS handshake failure"),
            Error::Closed => f.write_str("TLS connection closed"),
            Error::Busy => f.write_str("CA bundle slot in use"),
            Error::Flash(code) => write!(f, "flash error {code}"),
            Error::Psa(status) => write!(f, "PSA error {status}"),
        }
    }
}

impl From<devkit::OutOfBounds> for Error {
    fn from(_: devkit::OutOfBounds) -> Self {
        Error::Decode
    }
}

pub type Result<T> = core::result::Result<T, Error>;
