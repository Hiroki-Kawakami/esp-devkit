/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;

use crate::psa::{self, Key, Policy, sys};
use crate::{Error, Result, ct, rand, x25519};

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
}

enum Secret {
    X25519([u8; 32]),
    Psa(Key),
}

/// A key pair for one key agreement.
pub struct EphemeralSecret {
    secret: Secret,
    group: Group,
}

impl EphemeralSecret {
    fn policy(bits: usize) -> Policy {
        Policy {
            ty: psa::KEY_TYPE_ECC_KEY_PAIR_BASE | psa::ECC_FAMILY_SECP_R1,
            bits,
            usage: psa::USAGE_DERIVE,
            alg: psa::ALG_ECDH,
            alg2: 0,
        }
    }

    pub fn generate(group: Group) -> Result<Self> {
        let secret = match group {
            Group::X25519 => {
                let mut k = [0; 32];
                rand::fill(&mut k);
                Secret::X25519(k)
            }
            Group::P256 => Secret::Psa(Key::generate(&Self::policy(256))?),
            Group::P384 => Secret::Psa(Key::generate(&Self::policy(384))?),
        };
        Ok(Self { secret, group })
    }

    #[cfg(test)]
    pub(crate) fn from_private(group: Group, private: &[u8]) -> Result<Self> {
        let secret = match group {
            Group::X25519 => Secret::X25519(private.try_into().map_err(|_| Error::InvalidLength)?),
            Group::P256 => Secret::Psa(Key::import(&Self::policy(256), private)?),
            Group::P384 => Secret::Psa(Key::import(&Self::policy(384), private)?),
        };
        Ok(Self { secret, group })
    }

    pub fn group(&self) -> Group {
        self.group
    }

    pub fn public_key(&self) -> Result<Vec<u8>> {
        match &self.secret {
            Secret::X25519(k) => Ok(x25519::x25519(k, &x25519::BASE).to_vec()),
            Secret::Psa(key) => key.export_public(self.group.public_len()),
        }
    }

    /// Fails with `InvalidKey` for a peer key that is not on the curve, or an
    /// X25519 one of small order.
    pub fn agree(&self, peer: &[u8]) -> Result<Vec<u8>> {
        let key = match &self.secret {
            Secret::X25519(k) => {
                let peer: &[u8; 32] = peer.try_into().map_err(|_| Error::InvalidKey)?;
                let shared = x25519::x25519(k, peer);
                if ct::eq(&shared, &[0; 32]) {
                    return Err(Error::InvalidKey);
                }
                return Ok(shared.to_vec());
            }
            Secret::Psa(key) => key,
        };
        let mut out = vec![0; self.group.shared_len()];
        let mut len = 0;
        psa::result(unsafe {
            sys::psa_raw_key_agreement(psa::ALG_ECDH, key.id(), peer.as_ptr(), peer.len(), out.as_mut_ptr(), out.len(), &mut len)
        })?;
        out.truncate(len);
        Ok(out)
    }
}

impl Drop for EphemeralSecret {
    fn drop(&mut self) {
        if let Secret::X25519(k) = &mut self.secret {
            ct::zeroize(k);
        }
    }
}
