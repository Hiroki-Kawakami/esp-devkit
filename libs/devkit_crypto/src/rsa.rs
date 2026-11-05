/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use crate::der::{self, Reader};
use crate::hash::HashAlg;
use crate::psa::{self, Key, Policy};
use crate::{Error, Result};

const ALG_PSS_ANY: u32 = psa::ALG_RSA_PSS_BASE | (psa::ALG_ANY_HASH & 0xff);
const ALG_PKCS1_ANY: u32 = psa::ALG_RSA_PKCS1V15_SIGN_BASE | (psa::ALG_ANY_HASH & 0xff);
const MAX_DER: usize = 4096;

fn pss(hash: HashAlg) -> u32 {
    psa::ALG_RSA_PSS_BASE | (hash.psa() & 0xff)
}

fn pkcs1(hash: HashAlg) -> u32 {
    psa::ALG_RSA_PKCS1V15_SIGN_BASE | (hash.psa() & 0xff)
}

pub struct PrivateKey {
    key: Key,
}

impl PrivateKey {
    fn policy(bits: usize) -> Policy {
        Policy {
            ty: psa::KEY_TYPE_RSA_KEY_PAIR,
            bits,
            usage: psa::USAGE_SIGN_HASH | psa::USAGE_VERIFY_HASH | psa::USAGE_EXPORT,
            alg: ALG_PSS_ANY,
            alg2: ALG_PKCS1_ANY,
        }
    }

    /// With the public exponent 65537. Takes seconds on the device.
    pub fn generate(bits: usize) -> Result<Self> {
        Ok(Self { key: Key::generate(&Self::policy(bits))? })
    }

    /// A PKCS #1 RSAPrivateKey.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<Self> {
        Ok(Self { key: Key::import(&Self::policy(0), der)? })
    }

    pub fn to_pkcs1_der(&self) -> Result<Vec<u8>> {
        self.key.export(MAX_DER)
    }

    pub fn public_key(&self) -> Result<PublicKey> {
        PublicKey::from_pkcs1_der(&self.key.export_public(MAX_DER)?)
    }

    /// PSS with a salt as long as the hash, as TLS 1.3 requires.
    pub fn sign_pss(&self, hash: HashAlg, message: &[u8]) -> Result<Vec<u8>> {
        self.key.sign_message(pss(hash), message, MAX_DER / 8)
    }

    pub fn sign_pkcs1v15(&self, hash: HashAlg, message: &[u8]) -> Result<Vec<u8>> {
        self.key.sign_message(pkcs1(hash), message, MAX_DER / 8)
    }

    /// Signs `digest` as the `hash` of a message, without hashing it again.
    pub fn sign_pkcs1v15_prehashed(&self, hash: HashAlg, digest: &[u8]) -> Result<Vec<u8>> {
        if digest.len() != hash.output_len() {
            return Err(Error::InvalidLength);
        }
        self.key.sign_hash(pkcs1(hash), digest, MAX_DER / 8)
    }
}

pub struct PublicKey {
    key: Key,
    der: Vec<u8>,
}

impl PublicKey {
    /// A PKCS #1 RSAPublicKey.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<Self> {
        let policy =
            Policy { ty: psa::KEY_TYPE_RSA_PUBLIC_KEY, bits: 0, usage: psa::USAGE_VERIFY_HASH, alg: ALG_PSS_ANY, alg2: ALG_PKCS1_ANY };
        Ok(Self { key: Key::import(&policy, der)?, der: der.to_vec() })
    }

    pub fn to_pkcs1_der(&self) -> &[u8] {
        &self.der
    }

    /// Big-endian, without leading zeros.
    pub fn modulus(&self) -> &[u8] {
        self.parts().0
    }

    pub fn exponent(&self) -> &[u8] {
        self.parts().1
    }

    pub fn size(&self) -> usize {
        self.modulus().len()
    }

    fn parts(&self) -> (&[u8], &[u8]) {
        let parse = || -> Result<(&[u8], &[u8])> {
            let mut seq = Reader::new(&self.der).sequence()?;
            let n = der::unsigned(seq.read(der::INTEGER)?)?;
            let e = der::unsigned(seq.read(der::INTEGER)?)?;
            Ok((n, e))
        };
        parse().expect("PSA accepted the key")
    }

    pub fn verify_pss(&self, hash: HashAlg, message: &[u8], signature: &[u8]) -> Result<()> {
        self.key.verify_message(pss(hash), message, signature).map_err(verify_error)
    }

    pub fn verify_pkcs1v15(&self, hash: HashAlg, message: &[u8], signature: &[u8]) -> Result<()> {
        self.key.verify_message(pkcs1(hash), message, signature).map_err(verify_error)
    }
}

// A signature of the wrong length is INVALID_ARGUMENT to PSA.
fn verify_error(e: Error) -> Error {
    if e == Error::InvalidKey { Error::Verify } else { e }
}

impl Clone for PublicKey {
    fn clone(&self) -> Self {
        Self::from_pkcs1_der(&self.der).expect("re-importing a valid key")
    }
}
