/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod builder;
pub(crate) mod oid;
pub(crate) mod trust;
mod verify;

pub use builder::{CertificateBuilder, Name, Signer};
pub use trust::{Anchor, TrustStore};
pub use verify::{ServerName, verify_chain};

use crate::der::{self, Reader};
use crate::ecdsa::{self, Curve};
use crate::hash::HashAlg;
use crate::{CertError, ed25519, rsa};
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignatureAlgorithm {
    RsaPkcs1(HashAlg),
    /// RSASSA-PSS with MGF1 over the same hash and a salt as long as it.
    RsaPss(HashAlg),
    Ecdsa(HashAlg),
    Ed25519,
}

impl SignatureAlgorithm {
    /// From an AlgorithmIdentifier's content.
    pub fn from_der(alg_id: &[u8]) -> Result<Self> {
        let mut r = Reader::new(alg_id);
        let oid = r.read(der::OID)?;
        let alg = match oid {
            oid::SHA256_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha256),
            oid::SHA384_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha384),
            oid::SHA512_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha512),
            oid::SHA1_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha1),
            oid::ECDSA_WITH_SHA256 => SignatureAlgorithm::Ecdsa(HashAlg::Sha256),
            oid::ECDSA_WITH_SHA384 => SignatureAlgorithm::Ecdsa(HashAlg::Sha384),
            oid::ECDSA_WITH_SHA512 => SignatureAlgorithm::Ecdsa(HashAlg::Sha512),
            oid::ED25519 => SignatureAlgorithm::Ed25519,
            oid::RSASSA_PSS => return pss_params(&mut r.sequence()?).map(SignatureAlgorithm::RsaPss),
            _ => return Err(Error::Certificate(CertError::UnsupportedAlgorithm)),
        };
        if r.peek_tag() == Some(der::NULL) {
            r.read(der::NULL)?;
        }
        r.finish()?;
        Ok(alg)
    }
}

fn hash_from_oid(oid: &[u8]) -> Result<HashAlg> {
    match oid {
        oid::SHA256 => Ok(HashAlg::Sha256),
        oid::SHA384 => Ok(HashAlg::Sha384),
        oid::SHA512 => Ok(HashAlg::Sha512),
        _ => Err(Error::Certificate(CertError::UnsupportedAlgorithm)),
    }
}

fn pss_params(params: &mut Reader) -> Result<HashAlg> {
    let unsupported = Error::Certificate(CertError::UnsupportedAlgorithm);
    let hash = hash_from_oid(params.nested(der::context(0))?.sequence()?.read(der::OID)?)?;
    let mut mgf = params.nested(der::context(1))?.sequence()?;
    if mgf.read(der::OID)? != oid::MGF1 || hash_from_oid(mgf.sequence()?.read(der::OID)?)? != hash {
        return Err(unsupported);
    }
    let salt = params.nested(der::context(2))?.small_unsigned()?;
    if salt as usize != hash.output_len() {
        return Err(unsupported);
    }
    Ok(hash)
}

/// A subject public key, ready to verify with.
#[derive(Clone)]
pub enum PublicKey {
    Rsa(rsa::PublicKey),
    Ecdsa(ecdsa::VerifyingKey),
    Ed25519(ed25519::VerifyingKey),
}

impl PublicKey {
    /// From a whole SubjectPublicKeyInfo.
    pub fn from_spki(spki: &[u8]) -> Result<Self> {
        let unsupported = Error::Certificate(CertError::UnsupportedAlgorithm);
        let mut outer = Reader::new(spki);
        let mut info = outer.sequence()?;
        outer.finish()?;
        let mut alg = info.sequence()?;
        let key = info.bit_string_bytes()?;
        info.finish()?;
        match alg.read(der::OID)? {
            oid::RSA_ENCRYPTION => Ok(PublicKey::Rsa(rsa::PublicKey::from_pkcs1_der(key)?)),
            oid::EC_PUBLIC_KEY => {
                let curve = match alg.read(der::OID)? {
                    oid::PRIME256V1 => Curve::P256,
                    oid::SECP384R1 => Curve::P384,
                    oid::SECP521R1 => Curve::P521,
                    _ => return Err(unsupported),
                };
                Ok(PublicKey::Ecdsa(ecdsa::VerifyingKey::from_sec1(curve, key)?))
            }
            oid::ED25519 => {
                let bytes: &[u8; 32] = key.try_into().map_err(|_| Error::InvalidKey)?;
                Ok(PublicKey::Ed25519(ed25519::VerifyingKey::from_bytes(bytes)?))
            }
            _ => Err(unsupported),
        }
    }

    /// `signature` as X.509 and TLS carry it: DER for ECDSA.
    pub fn verify(&self, alg: SignatureAlgorithm, message: &[u8], signature: &[u8]) -> Result<()> {
        match (self, alg) {
            (PublicKey::Rsa(key), SignatureAlgorithm::RsaPkcs1(hash)) => key.verify_pkcs1v15(hash, message, signature),
            (PublicKey::Rsa(key), SignatureAlgorithm::RsaPss(hash)) => key.verify_pss(hash, message, signature),
            (PublicKey::Ecdsa(key), SignatureAlgorithm::Ecdsa(hash)) => key.verify_der(hash, message, signature),
            (PublicKey::Ed25519(key), SignatureAlgorithm::Ed25519) => key.verify(message, signature),
            _ => Err(Error::Verify),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BasicConstraints {
    pub ca: bool,
    pub path_len: Option<u32>,
}

pub const KEY_USAGE_DIGITAL_SIGNATURE: u16 = 1 << 0;
pub const KEY_USAGE_KEY_CERT_SIGN: u16 = 1 << 5;

/// A parsed certificate borrowing its DER.
#[derive(Clone, Copy, Debug)]
pub struct Certificate<'a> {
    pub der: &'a [u8],
    pub tbs: &'a [u8],
    pub signature_algorithm: SignatureAlgorithm,
    pub signature: &'a [u8],
    pub serial: &'a [u8],
    /// Whole Name encodings.
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    /// Unix seconds.
    pub not_before: i64,
    pub not_after: i64,
    /// The whole SubjectPublicKeyInfo.
    pub spki: &'a [u8],
    pub basic_constraints: Option<BasicConstraints>,
    /// Bit `n` is KeyUsage bit `n` (`KEY_USAGE_*`).
    pub key_usage: Option<u16>,
    /// The content of the extensions, when present.
    pub ext_key_usage: Option<&'a [u8]>,
    pub subject_alt_names: Option<&'a [u8]>,
    pub name_constraints: Option<&'a [u8]>,
}

impl<'a> Certificate<'a> {
    /// Fails with `UnknownCritical` on a critical extension it does not know.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut outer = Reader::new(data);
        let der = outer.read_whole(der::SEQUENCE)?;
        outer.finish()?;
        let mut cert = Reader::new(der).sequence()?;
        let tbs = cert.read_whole(der::SEQUENCE)?;
        let signature_algorithm = SignatureAlgorithm::from_der(cert.read(der::SEQUENCE)?);
        let signature = cert.bit_string_bytes()?;
        cert.finish()?;

        let mut t = Reader::new(tbs).sequence()?;
        let version = match t.read_optional(der::context(0))? {
            Some(v) => Reader::new(v).small_unsigned()?,
            None => 0,
        };
        let serial = t.read(der::INTEGER)?;
        let inner_alg = t.read(der::SEQUENCE)?;
        let issuer = t.read_whole(der::SEQUENCE)?;
        let mut validity = t.sequence()?;
        let not_before = time(&mut validity)?;
        let not_after = time(&mut validity)?;
        validity.finish()?;
        let subject = t.read_whole(der::SEQUENCE)?;
        let spki = t.read_whole(der::SEQUENCE)?;
        t.read_optional(der::context_primitive(1))?;
        t.read_optional(der::context_primitive(2))?;
        let mut parsed = Certificate {
            der,
            tbs,
            signature_algorithm: SignatureAlgorithm::RsaPkcs1(HashAlg::Sha256),
            signature,
            serial,
            issuer,
            subject,
            not_before,
            not_after,
            spki,
            basic_constraints: None,
            key_usage: None,
            ext_key_usage: None,
            subject_alt_names: None,
            name_constraints: None,
        };
        if let Some(exts) = t.read_optional(der::context(3))? {
            if version != 2 {
                return Err(Error::Decode);
            }
            parsed.extensions(exts)?;
        }
        t.finish()?;
        let inner = SignatureAlgorithm::from_der(inner_alg);
        parsed.signature_algorithm = match (signature_algorithm, inner) {
            (Ok(a), Ok(b)) if a == b => a,
            (Ok(_), Ok(_)) => return Err(Error::Decode),
            (Err(e), _) | (_, Err(e)) => return Err(e),
        };
        Ok(parsed)
    }

    fn extensions(&mut self, exts: &'a [u8]) -> Result<()> {
        let mut list = Reader::new(exts).sequence()?;
        while !list.is_empty() {
            let mut ext = list.sequence()?;
            let id = ext.read(der::OID)?;
            let critical = if ext.peek_tag() == Some(der::BOOLEAN) { ext.boolean()? } else { false };
            let value = ext.read(der::OCTET_STRING)?;
            ext.finish()?;
            match id {
                oid::BASIC_CONSTRAINTS => {
                    let mut seq = Reader::new(value).sequence()?;
                    let ca = if seq.peek_tag() == Some(der::BOOLEAN) { seq.boolean()? } else { false };
                    let path_len = if seq.is_empty() { None } else { Some(seq.small_unsigned()?) };
                    seq.finish()?;
                    self.basic_constraints = Some(BasicConstraints { ca, path_len });
                }
                oid::KEY_USAGE => {
                    let bits = Reader::new(value).read(der::BIT_STRING)?;
                    let (&unused, bytes) = bits.split_first().ok_or(Error::Decode)?;
                    if unused > 7 {
                        return Err(Error::Decode);
                    }
                    let mut usage = 0u16;
                    for (i, &byte) in bytes.iter().take(2).enumerate() {
                        usage |= (byte.reverse_bits() as u16) << (8 * i);
                    }
                    self.key_usage = Some(usage);
                }
                oid::EXT_KEY_USAGE => self.ext_key_usage = Some(value),
                oid::SUBJECT_ALT_NAME => self.subject_alt_names = Some(value),
                oid::NAME_CONSTRAINTS => self.name_constraints = Some(value),
                oid::SUBJECT_KEY_ID
                | oid::AUTHORITY_KEY_ID
                | oid::CERTIFICATE_POLICIES
                | oid::POLICY_CONSTRAINTS
                | oid::POLICY_MAPPINGS
                | oid::INHIBIT_ANY_POLICY
                | oid::CRL_DISTRIBUTION_POINTS => {}
                _ if critical => return Err(Error::Certificate(CertError::UnknownCritical)),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn public_key(&self) -> Result<PublicKey> {
        PublicKey::from_spki(self.spki)
    }

    /// Whether `issuer`'s key signed this certificate.
    pub fn verify_signed_by(&self, issuer: &PublicKey) -> Result<()> {
        issuer
            .verify(self.signature_algorithm, self.tbs, self.signature)
            .map_err(|_| Error::Certificate(CertError::BadSignature))
    }

    /// The dNSName entries of the subject alternative names.
    pub fn dns_names(&self) -> impl Iterator<Item = &'a [u8]> {
        general_names(self.subject_alt_names, der::context_primitive(2))
    }

    pub fn ip_addresses(&self) -> impl Iterator<Item = &'a [u8]> {
        general_names(self.subject_alt_names, der::context_primitive(7))
    }

    pub fn has_ext_key_usage(&self, purpose: &[u8]) -> bool {
        let Some(value) = self.ext_key_usage else { return true };
        let Ok(mut seq) = Reader::new(value).sequence() else { return false };
        while let Ok(id) = seq.read(der::OID) {
            if id == purpose || id == oid::ANY_EXT_KEY_USAGE {
                return true;
            }
        }
        false
    }
}

fn general_names(value: Option<&[u8]>, tag: u8) -> impl Iterator<Item = &[u8]> {
    let mut seq = value.and_then(|v| Reader::new(v).sequence().ok()).unwrap_or(Reader::new(&[]));
    core::iter::from_fn(move || {
        while let Ok((t, content, _)) = seq.read_any() {
            if t == tag {
                return Some(content);
            }
        }
        None
    })
}

fn time(r: &mut Reader) -> Result<i64> {
    let (tag, content, _) = r.read_any()?;
    let digits = |s: &[u8]| -> Result<i64> {
        s.iter().try_fold(0i64, |acc, &c| if c.is_ascii_digit() { Ok(acc * 10 + (c - b'0') as i64) } else { Err(Error::Decode) })
    };
    let (year, rest) = match (tag, content.len()) {
        (der::UTC_TIME, 13) => {
            let yy = digits(&content[..2])?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &content[2..])
        }
        (der::GENERALIZED_TIME, 15) => (digits(&content[..4])?, &content[4..]),
        _ => return Err(Error::Decode),
    };
    if rest[10] != b'Z' {
        return Err(Error::Decode);
    }
    let [month, day, hour, minute, second] = [0, 2, 4, 6, 8].map(|i| digits(&rest[i..i + 2]));
    let (month, day, hour, minute, second) = (month?, day?, hour?, minute?, second?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return Err(Error::Decode);
    }
    Ok(days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second)
}

pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
