/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod client;
mod schedule;

pub use client::Connection;

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use idf::heap::Caps;

use crate::aead::Algorithm;
use crate::ecdh::Group;
use crate::x509::TrustStore;
use crate::{Result, ecdsa, ed25519, rsa};
use schedule::HashKind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CipherSuite {
    Aes128GcmSha256,
    Aes256GcmSha384,
    ChaCha20Poly1305Sha256,
}

impl CipherSuite {
    pub fn id(self) -> u16 {
        match self {
            CipherSuite::Aes128GcmSha256 => 0x1301,
            CipherSuite::Aes256GcmSha384 => 0x1302,
            CipherSuite::ChaCha20Poly1305Sha256 => 0x1303,
        }
    }

    fn from_id(id: u16) -> Option<Self> {
        [CipherSuite::Aes128GcmSha256, CipherSuite::Aes256GcmSha384, CipherSuite::ChaCha20Poly1305Sha256]
            .into_iter()
            .find(|s| s.id() == id)
    }

    fn aead(self) -> Algorithm {
        match self {
            CipherSuite::Aes128GcmSha256 => Algorithm::Aes128Gcm,
            CipherSuite::Aes256GcmSha384 => Algorithm::Aes256Gcm,
            CipherSuite::ChaCha20Poly1305Sha256 => Algorithm::ChaCha20Poly1305,
        }
    }

    fn hash(self) -> HashKind {
        match self {
            CipherSuite::Aes256GcmSha384 => HashKind::Sha384,
            _ => HashKind::Sha256,
        }
    }
}

pub enum PrivateKey {
    Rsa(rsa::PrivateKey),
    Ecdsa(ecdsa::SigningKey),
    Ed25519(ed25519::SigningKey),
}

/// A certificate chain, leaf first, and the leaf's private key.
pub struct Identity {
    pub chain: Vec<Vec<u8>>,
    pub key: PrivateKey,
}

/// Decides whether the server's chain, leaf first, is acceptable. The
/// connection checks the server's CertificateVerify signature itself.
pub trait Verifier: Send + Sync {
    fn verify(&self, chain: &[Vec<u8>], server_name: Option<&str>) -> Result<()>;
}

pub enum Verify {
    /// A chain to one of the store's roots, naming the server.
    WebPki(TrustStore),
    /// A leaf with exactly this SubjectPublicKeyInfo.
    PinnedSpki(Vec<u8>),
    Custom(Arc<dyn Verifier>),
    /// Any certificate; for protocols that authenticate the peer some other way.
    None,
}

pub struct ClientConfig {
    pub verify: Verify,
    /// Unix seconds now, for certificate validity periods; `None` skips them.
    pub clock: Option<fn() -> i64>,
    /// Offered in order; empty sends no ALPN.
    pub alpn: Vec<Vec<u8>>,
    /// Sent when the server asks for a certificate.
    pub identity: Option<Identity>,
    /// In order of preference.
    pub cipher_suites: Vec<CipherSuite>,
    /// The first gets a key share in the ClientHello.
    pub groups: Vec<Group>,
    /// Where the 16.6 KB receive buffer lives.
    pub buffer_caps: Caps,
}

impl ClientConfig {
    pub fn new(verify: Verify) -> Self {
        Self {
            verify,
            clock: None,
            alpn: Vec::new(),
            identity: None,
            cipher_suites: vec![
                CipherSuite::Aes128GcmSha256,
                CipherSuite::ChaCha20Poly1305Sha256,
                CipherSuite::Aes256GcmSha384,
            ],
            groups: vec![Group::X25519, Group::P256, Group::P384],
            buffer_caps: Caps::DEFAULT,
        }
    }
}

pub(crate) mod alert {
    pub const CLOSE_NOTIFY: u8 = 0;
    pub const UNEXPECTED_MESSAGE: u8 = 10;
    pub const BAD_RECORD_MAC: u8 = 20;
    pub const RECORD_OVERFLOW: u8 = 22;
    pub const HANDSHAKE_FAILURE: u8 = 40;
    pub const BAD_CERTIFICATE: u8 = 42;
    pub const UNSUPPORTED_CERTIFICATE: u8 = 43;
    pub const CERTIFICATE_EXPIRED: u8 = 45;
    pub const ILLEGAL_PARAMETER: u8 = 47;
    pub const UNKNOWN_CA: u8 = 48;
    pub const DECODE_ERROR: u8 = 50;
    pub const DECRYPT_ERROR: u8 = 51;
    pub const PROTOCOL_VERSION: u8 = 70;
    pub const INTERNAL_ERROR: u8 = 80;
    pub const USER_CANCELED: u8 = 90;
    pub const MISSING_EXTENSION: u8 = 109;
    pub const UNSUPPORTED_EXTENSION: u8 = 110;
}
