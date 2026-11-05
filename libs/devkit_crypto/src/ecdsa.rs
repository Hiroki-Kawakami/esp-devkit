/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use crate::der::{self, Reader, Writer};
use crate::hash::HashAlg;
use crate::psa::{self, Key, Policy};
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Curve {
    P256,
    P384,
    P521,
}

impl Curve {
    pub fn scalar_len(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
            Curve::P521 => 66,
        }
    }

    fn bits(self) -> usize {
        match self {
            Curve::P521 => 521,
            _ => self.scalar_len() * 8,
        }
    }

    /// An uncompressed SEC 1 point.
    pub fn point_len(self) -> usize {
        1 + 2 * self.scalar_len()
    }

    /// The hash this curve is paired with in TLS 1.3.
    pub fn hash(self) -> HashAlg {
        match self {
            Curve::P256 => HashAlg::Sha256,
            Curve::P384 => HashAlg::Sha384,
            Curve::P521 => HashAlg::Sha512,
        }
    }
}

const ALG_ANY: u32 = psa::ALG_ECDSA_BASE | (psa::ALG_ANY_HASH & 0xff);

fn alg(hash: HashAlg) -> u32 {
    psa::ALG_ECDSA_BASE | (hash.psa() & 0xff)
}

pub struct SigningKey {
    key: Key,
    curve: Curve,
}

impl SigningKey {
    fn policy(curve: Curve) -> Policy {
        Policy {
            ty: psa::KEY_TYPE_ECC_KEY_PAIR_BASE | psa::ECC_FAMILY_SECP_R1,
            bits: curve.bits(),
            usage: psa::USAGE_SIGN_HASH | psa::USAGE_VERIFY_HASH | psa::USAGE_EXPORT,
            alg: ALG_ANY,
            alg2: 0,
        }
    }

    pub fn generate(curve: Curve) -> Result<Self> {
        Ok(Self { key: Key::generate(&Self::policy(curve))?, curve })
    }

    /// The private scalar, big-endian.
    pub fn from_bytes(curve: Curve, scalar: &[u8]) -> Result<Self> {
        if scalar.len() != curve.scalar_len() {
            return Err(Error::InvalidLength);
        }
        Ok(Self { key: Key::import(&Self::policy(curve), scalar)?, curve })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.key.export(self.curve.scalar_len())
    }

    pub fn curve(&self) -> Curve {
        self.curve
    }

    pub fn verifying_key(&self) -> Result<VerifyingKey> {
        VerifyingKey::from_sec1(self.curve, &self.key.export_public(self.curve.point_len())?)
    }

    /// `r || s`, each `scalar_len()` bytes.
    pub fn sign(&self, hash: HashAlg, message: &[u8]) -> Result<Vec<u8>> {
        self.key.sign_message(alg(hash), message, 2 * self.curve.scalar_len())
    }
}

pub struct VerifyingKey {
    key: Key,
    curve: Curve,
    point: Vec<u8>,
}

impl VerifyingKey {
    /// An uncompressed SEC 1 point.
    pub fn from_sec1(curve: Curve, point: &[u8]) -> Result<Self> {
        if point.len() != curve.point_len() {
            return Err(Error::InvalidKey);
        }
        let policy = Policy {
            ty: psa::KEY_TYPE_ECC_PUBLIC_KEY_BASE | psa::ECC_FAMILY_SECP_R1,
            bits: curve.bits(),
            usage: psa::USAGE_VERIFY_HASH,
            alg: ALG_ANY,
            alg2: 0,
        };
        Ok(Self { key: Key::import(&policy, point)?, curve, point: point.to_vec() })
    }

    pub fn curve(&self) -> Curve {
        self.curve
    }

    pub fn to_sec1(&self) -> &[u8] {
        &self.point
    }

    /// `signature` is `r || s`.
    pub fn verify(&self, hash: HashAlg, message: &[u8], signature: &[u8]) -> Result<()> {
        if signature.len() != 2 * self.curve.scalar_len() {
            return Err(Error::Verify);
        }
        self.key.verify_message(alg(hash), message, signature)
    }

    /// `signature` is a DER Ecdsa-Sig-Value, as X.509 and TLS carry it.
    pub fn verify_der(&self, hash: HashAlg, message: &[u8], signature: &[u8]) -> Result<()> {
        let raw = signature_from_der(self.curve, signature).map_err(|_| Error::Verify)?;
        self.verify(hash, message, &raw)
    }
}

impl Clone for VerifyingKey {
    fn clone(&self) -> Self {
        Self::from_sec1(self.curve, &self.point).expect("re-importing a valid key")
    }
}

/// `r || s` to a DER Ecdsa-Sig-Value.
pub fn signature_to_der(raw: &[u8]) -> Vec<u8> {
    let (r, s) = raw.split_at(raw.len() / 2);
    let mut w = Writer::new();
    w.sequence(|w| {
        w.unsigned_integer(r);
        w.unsigned_integer(s);
    });
    w.finish()
}

pub fn signature_from_der(curve: Curve, signature: &[u8]) -> Result<Vec<u8>> {
    let n = curve.scalar_len();
    let mut outer = Reader::new(signature);
    let mut seq = outer.sequence()?;
    outer.finish()?;
    let mut raw = alloc::vec![0; 2 * n];
    for half in raw.chunks_mut(n) {
        let value = der::unsigned(seq.read(der::INTEGER)?)?;
        if value.len() > n {
            return Err(Error::Decode);
        }
        half[n - value.len()..].copy_from_slice(value);
    }
    seq.finish()?;
    Ok(raw)
}
