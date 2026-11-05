/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::hash::Hash;
use crate::mac::Hmac;
use crate::{Error, Result};

/// HKDF (RFC 5869) over any [`Hash`].
#[derive(Clone)]
pub struct Hkdf<H: Hash> {
    prk: Hmac<H>,
}

impl<H: Hash> Hkdf<H> {
    pub fn new(salt: &[u8], ikm: &[u8]) -> Self {
        Self::from_prk(Self::extract(salt, ikm).as_ref())
    }

    pub fn extract(salt: &[u8], ikm: &[u8]) -> H::Output {
        Hmac::<H>::mac(salt, ikm)
    }

    pub fn from_prk(prk: &[u8]) -> Self {
        Self { prk: Hmac::new(prk) }
    }

    /// `info` is the concatenation of its parts. At most 255 hash lengths.
    pub fn expand(&self, info: &[&[u8]], out: &mut [u8]) -> Result<()> {
        let hash_len = H::ZERO.as_ref().len();
        if out.len() > 255 * hash_len {
            return Err(Error::InvalidLength);
        }
        let mut previous = H::ZERO;
        for (i, chunk) in out.chunks_mut(hash_len).enumerate() {
            let mut mac = self.prk.clone();
            if i > 0 {
                mac.update(previous.as_ref());
            }
            for part in info {
                mac.update(part);
            }
            mac.update(&[i as u8 + 1]);
            previous = mac.finalize();
            chunk.copy_from_slice(&previous.as_ref()[..chunk.len()]);
        }
        Ok(())
    }
}
