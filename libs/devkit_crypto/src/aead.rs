/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::psa::{self, Key, Policy, sys};
use crate::{Error, Result};

pub const NONCE: usize = 12;
pub const TAG: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Algorithm {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

impl Algorithm {
    pub fn key_len(self) -> usize {
        match self {
            Algorithm::Aes128Gcm => 16,
            Algorithm::Aes256Gcm | Algorithm::ChaCha20Poly1305 => 32,
        }
    }

    fn psa(self) -> (u16, u32) {
        match self {
            Algorithm::Aes128Gcm | Algorithm::Aes256Gcm => (psa::KEY_TYPE_AES, psa::ALG_GCM),
            Algorithm::ChaCha20Poly1305 => (psa::KEY_TYPE_CHACHA20, psa::ALG_CHACHA20_POLY1305),
        }
    }
}

/// An AEAD key, with a 12 byte nonce and a 16 byte tag after the ciphertext.
pub struct Aead {
    key: Key,
    alg: u32,
    algorithm: Algorithm,
}

impl Aead {
    pub fn new(algorithm: Algorithm, key: &[u8]) -> Result<Self> {
        if key.len() != algorithm.key_len() {
            return Err(Error::InvalidLength);
        }
        let (ty, alg) = algorithm.psa();
        let policy = Policy { ty, bits: key.len() * 8, usage: psa::USAGE_ENCRYPT | psa::USAGE_DECRYPT, alg, alg2: 0 };
        Ok(Self { key: Key::import(&policy, key)?, alg, algorithm })
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Encrypts the first `len` bytes of `buf` and writes the tag after them;
    /// `buf` must hold `len + TAG` bytes.
    // In place, as mbedTLS's own TLS does: IDF builds PSA with
    // MBEDTLS_PSA_ASSUME_EXCLUSIVE_BUFFERS, so only exact aliasing is safe.
    pub fn seal_in_place(&self, nonce: &[u8; NONCE], aad: &[u8], buf: &mut [u8], len: usize) -> Result<()> {
        if buf.len() < len + TAG {
            return Err(Error::InvalidLength);
        }
        let mut out = 0;
        let ptr = buf.as_mut_ptr();
        psa::result(unsafe {
            sys::psa_aead_encrypt(
                self.key.id(),
                self.alg,
                nonce.as_ptr(),
                NONCE,
                aad.as_ptr(),
                aad.len(),
                ptr,
                len,
                ptr,
                len + TAG,
                &mut out,
            )
        })
    }

    /// Decrypts ciphertext followed by its tag in place; returns the plaintext.
    pub fn open_in_place<'a>(&self, nonce: &[u8; NONCE], aad: &[u8], buf: &'a mut [u8]) -> Result<&'a mut [u8]> {
        if buf.len() < TAG {
            return Err(Error::InvalidLength);
        }
        let mut out = 0;
        let ptr = buf.as_mut_ptr();
        psa::result(unsafe {
            sys::psa_aead_decrypt(
                self.key.id(),
                self.alg,
                nonce.as_ptr(),
                NONCE,
                aad.as_ptr(),
                aad.len(),
                ptr,
                buf.len(),
                ptr,
                buf.len() - TAG,
                &mut out,
            )
        })?;
        Ok(&mut buf[..out])
    }
}
