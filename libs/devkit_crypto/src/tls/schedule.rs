/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use crate::ct;
use crate::hash::{Hash, Sha256, Sha384};
use crate::kdf::Hkdf;
use crate::mac::Hmac;

/// A secret of up to 48 bytes, cleared on drop.
#[derive(Clone)]
pub(crate) struct Secret {
    buf: [u8; 48],
    len: usize,
}

impl Secret {
    pub fn new(bytes: &[u8]) -> Self {
        let mut buf = [0; 48];
        buf[..bytes.len()].copy_from_slice(bytes);
        Self { buf, len: bytes.len() }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        ct::zeroize(&mut self.buf);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HashKind {
    Sha256,
    Sha384,
}

impl HashKind {
    pub fn len(self) -> usize {
        match self {
            HashKind::Sha256 => 32,
            HashKind::Sha384 => 48,
        }
    }

    pub fn digest(self, data: &[u8]) -> Secret {
        let mut t = Transcript::new(self);
        t.update(data);
        t.current()
    }

    pub fn extract(self, salt: &[u8], ikm: &[u8]) -> Secret {
        match self {
            HashKind::Sha256 => Secret::new(&Hkdf::<Sha256>::extract(salt, ikm)),
            HashKind::Sha384 => Secret::new(&Hkdf::<Sha384>::extract(salt, ikm)),
        }
    }

    pub fn hmac(self, key: &[u8], data: &[u8]) -> Secret {
        match self {
            HashKind::Sha256 => Secret::new(&Hmac::<Sha256>::mac(key, data)),
            HashKind::Sha384 => Secret::new(&Hmac::<Sha384>::mac(key, data)),
        }
    }

    /// HKDF-Expand-Label (RFC 8446 section 7.1).
    pub fn expand_label(self, secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
        let mut info = Vec::with_capacity(4 + 6 + label.len() + context.len());
        info.extend_from_slice(&(out.len() as u16).to_be_bytes());
        info.push((6 + label.len()) as u8);
        info.extend_from_slice(b"tls13 ");
        info.extend_from_slice(label);
        info.push(context.len() as u8);
        info.extend_from_slice(context);
        let done = match self {
            HashKind::Sha256 => Hkdf::<Sha256>::from_prk(secret).expand(&[&info], out),
            HashKind::Sha384 => Hkdf::<Sha384>::from_prk(secret).expand(&[&info], out),
        };
        done.expect("TLS labels stay short");
    }

    pub fn expand_secret(self, secret: &[u8], label: &[u8], context: &[u8]) -> Secret {
        let mut out = [0; 48];
        let out = &mut out[..self.len()];
        self.expand_label(secret, label, context, out);
        let secret = Secret::new(out);
        ct::zeroize(out);
        secret
    }

    /// Derive-Secret with the hash of `messages` already taken.
    pub fn derive(self, secret: &Secret, label: &[u8], transcript: &Secret) -> Secret {
        self.expand_secret(secret.as_bytes(), label, transcript.as_bytes())
    }

    pub fn empty_hash(self) -> Secret {
        self.digest(&[])
    }
}

#[derive(Clone)]
pub(crate) enum Transcript {
    Sha256(Sha256),
    Sha384(Sha384),
}

impl Transcript {
    pub fn new(kind: HashKind) -> Self {
        match kind {
            HashKind::Sha256 => Transcript::Sha256(Sha256::new()),
            HashKind::Sha384 => Transcript::Sha384(Sha384::new()),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            Transcript::Sha256(h) => h.update(data),
            Transcript::Sha384(h) => h.update(data),
        }
    }

    /// The hash so far; the transcript goes on.
    pub fn current(&self) -> Secret {
        match self.clone() {
            Transcript::Sha256(h) => Secret::new(&h.finalize()),
            Transcript::Sha384(h) => Secret::new(&h.finalize()),
        }
    }
}
