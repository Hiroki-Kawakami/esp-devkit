/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;

use crate::psa::{self, Key, Policy, sys};
use crate::Result;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    X25519,
    P256,
    P384,
}

impl Group {
    /// The encoded public key: 32 bytes for X25519, an uncompressed point otherwise.
    pub fn public_len(self) -> usize {
        match self {
            Group::X25519 => 32,
            Group::P256 => 65,
            Group::P384 => 97,
        }
    }

    pub fn shared_len(self) -> usize {
        match self {
            Group::X25519 | Group::P256 => 32,
            Group::P384 => 48,
        }
    }

    fn psa(self) -> (u16, usize) {
        match self {
            Group::X25519 => (psa::ECC_FAMILY_MONTGOMERY, 255),
            Group::P256 => (psa::ECC_FAMILY_SECP_R1, 256),
            Group::P384 => (psa::ECC_FAMILY_SECP_R1, 384),
        }
    }
}

/// A key pair for one key agreement.
pub struct EphemeralSecret {
    key: Key,
    group: Group,
}

impl EphemeralSecret {
    fn policy(group: Group) -> Policy {
        let (family, bits) = group.psa();
        Policy { ty: psa::KEY_TYPE_ECC_KEY_PAIR_BASE | family, bits, usage: psa::USAGE_DERIVE, alg: psa::ALG_ECDH, alg2: 0 }
    }

    pub fn generate(group: Group) -> Result<Self> {
        Ok(Self { key: Key::generate(&Self::policy(group))?, group })
    }

    #[cfg(test)]
    pub(crate) fn from_private(group: Group, private: &[u8]) -> Result<Self> {
        Ok(Self { key: Key::import(&Self::policy(group), private)?, group })
    }

    pub fn group(&self) -> Group {
        self.group
    }

    pub fn public_key(&self) -> Result<Vec<u8>> {
        self.key.export_public(self.group.public_len())
    }

    /// Fails with `InvalidKey` for a peer key that is not on the curve.
    pub fn agree(&self, peer: &[u8]) -> Result<Vec<u8>> {
        let mut out = vec![0; self.group.shared_len()];
        let mut len = 0;
        psa::result(unsafe {
            sys::psa_raw_key_agreement(
                psa::ALG_ECDH,
                self.key.id(),
                peer.as_ptr(),
                peer.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        })?;
        out.truncate(len);
        Ok(out)
    }
}
