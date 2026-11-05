/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

use super::oid;
use crate::der::{self, Writer};
use crate::ecdsa::{self, Curve};
use crate::hash::HashAlg;
use crate::{Result, ed25519, rsa};

/// A distinguished name, one attribute per RDN, in the order added.
#[derive(Clone, Default)]
pub struct Name {
    rdns: Vec<u8>,
}

impl Name {
    pub fn new() -> Self {
        Self::default()
    }

    fn attribute(mut self, id: &[u8], tag: u8, value: &str) -> Self {
        let mut w = Writer::new();
        w.nested(der::SET, |w| {
            w.sequence(|w| {
                w.oid(id);
                w.tlv(tag, value.as_bytes());
            })
        });
        self.rdns.extend_from_slice(&w.finish());
        self
    }

    pub fn common_name(self, value: &str) -> Self {
        self.attribute(oid::COMMON_NAME, der::UTF8_STRING, value)
    }

    pub fn organization(self, value: &str) -> Self {
        self.attribute(oid::ORGANIZATION, der::UTF8_STRING, value)
    }

    pub fn organizational_unit(self, value: &str) -> Self {
        self.attribute(oid::ORGANIZATIONAL_UNIT, der::UTF8_STRING, value)
    }

    /// Two letters.
    pub fn country(self, value: &str) -> Self {
        self.attribute(oid::COUNTRY, der::PRINTABLE_STRING, value)
    }

    pub fn to_der(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.tlv(der::SEQUENCE, &self.rdns);
        w.finish()
    }
}

/// The key a certificate is signed with: SHA-256 with PKCS #1 v1.5 for RSA,
/// the curve's own hash for ECDSA.
#[derive(Clone, Copy)]
pub enum Signer<'a> {
    Rsa(&'a rsa::PrivateKey),
    Ecdsa(&'a ecdsa::SigningKey),
    Ed25519(&'a ed25519::SigningKey),
}

impl Signer<'_> {
    /// The SubjectPublicKeyInfo of the signer's own key.
    pub fn spki(&self) -> Result<Vec<u8>> {
        let mut w = Writer::new();
        match self {
            Signer::Rsa(key) => {
                let public = key.public_key()?;
                w.sequence(|w| {
                    w.sequence(|w| {
                        w.oid(oid::RSA_ENCRYPTION);
                        w.null();
                    });
                    w.bit_string(public.to_pkcs1_der());
                });
            }
            Signer::Ecdsa(key) => {
                let public = key.verifying_key()?;
                w.sequence(|w| {
                    w.sequence(|w| {
                        w.oid(oid::EC_PUBLIC_KEY);
                        w.oid(match key.curve() {
                            Curve::P256 => oid::PRIME256V1,
                            Curve::P384 => oid::SECP384R1,
                            Curve::P521 => oid::SECP521R1,
                        });
                    });
                    w.bit_string(public.to_sec1());
                });
            }
            Signer::Ed25519(key) => {
                w.sequence(|w| {
                    w.sequence(|w| w.oid(oid::ED25519));
                    w.bit_string(&key.verifying_key().to_bytes());
                });
            }
        }
        Ok(w.finish())
    }

    fn algorithm(&self, w: &mut Writer) {
        w.sequence(|w| match self {
            Signer::Rsa(_) => {
                w.oid(oid::SHA256_WITH_RSA);
                w.null();
            }
            Signer::Ecdsa(key) => w.oid(match key.curve() {
                Curve::P256 => oid::ECDSA_WITH_SHA256,
                Curve::P384 => oid::ECDSA_WITH_SHA384,
                Curve::P521 => oid::ECDSA_WITH_SHA512,
            }),
            Signer::Ed25519(_) => w.oid(oid::ED25519),
        });
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>> {
        match self {
            Signer::Rsa(key) => key.sign_pkcs1v15(HashAlg::Sha256, message),
            Signer::Ecdsa(key) => Ok(ecdsa::signature_to_der(&key.sign(key.curve().hash(), message)?)),
            Signer::Ed25519(key) => Ok(key.sign(message).to_vec()),
        }
    }
}

/// Builds a version 3 certificate.
pub struct CertificateBuilder {
    subject: Name,
    serial: Vec<u8>,
    not_before: i64,
    not_after: i64,
    ca: Option<bool>,
    dns_names: Vec<String>,
}

impl CertificateBuilder {
    /// Serial 1, valid from 2000 to 9999 unless set.
    pub fn new(subject: Name) -> Self {
        Self {
            subject,
            serial: alloc::vec![1],
            not_before: 946_684_800,
            not_after: 253_402_300_799,
            ca: None,
            dns_names: Vec::new(),
        }
    }

    /// Big-endian.
    pub fn serial(mut self, serial: &[u8]) -> Self {
        self.serial = serial.to_vec();
        self
    }

    /// Unix seconds.
    pub fn validity(mut self, not_before: i64, not_after: i64) -> Self {
        self.not_before = not_before;
        self.not_after = not_after;
        self
    }

    /// Adds basicConstraints, critical.
    pub fn ca(mut self, ca: bool) -> Self {
        self.ca = Some(ca);
        self
    }

    pub fn dns_name(mut self, name: &str) -> Self {
        self.dns_names.push(String::from(name));
        self
    }

    pub fn self_signed(&self, signer: Signer) -> Result<Vec<u8>> {
        self.sign(&signer.spki()?, &self.subject, signer)
    }

    /// For `subject_spki`, signed by `signer` as `issuer`.
    pub fn sign(&self, subject_spki: &[u8], issuer: &Name, signer: Signer) -> Result<Vec<u8>> {
        let mut tbs = Writer::new();
        tbs.sequence(|w| {
            w.nested(der::context(0), |w| w.small_integer(2));
            w.unsigned_integer(&self.serial);
            signer.algorithm(w);
            w.raw(&issuer.to_der());
            w.sequence(|w| {
                time(w, self.not_before);
                time(w, self.not_after);
            });
            w.raw(&self.subject.to_der());
            w.raw(subject_spki);
            if self.ca.is_some() || !self.dns_names.is_empty() {
                w.nested(der::context(3), |w| {
                    w.sequence(|w| {
                        if let Some(ca) = self.ca {
                            w.sequence(|w| {
                                w.oid(oid::BASIC_CONSTRAINTS);
                                w.boolean(true);
                                w.nested(der::OCTET_STRING, |w| {
                                    w.sequence(|w| {
                                        if ca {
                                            w.boolean(true);
                                        }
                                    })
                                });
                            });
                        }
                        if !self.dns_names.is_empty() {
                            w.sequence(|w| {
                                w.oid(oid::SUBJECT_ALT_NAME);
                                w.nested(der::OCTET_STRING, |w| {
                                    w.sequence(|w| {
                                        for name in &self.dns_names {
                                            w.tlv(der::context_primitive(2), name.as_bytes());
                                        }
                                    })
                                });
                            });
                        }
                    })
                });
            }
        });
        let tbs = tbs.finish();
        let signature = signer.sign(&tbs)?;
        let mut cert = Writer::new();
        cert.sequence(|w| {
            w.raw(&tbs);
            signer.algorithm(w);
            w.bit_string(&signature);
        });
        Ok(cert.finish())
    }
}

fn time(w: &mut Writer, unix: i64) {
    let days = unix.div_euclid(86400);
    let secs = unix.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let text = alloc::format!("{:02}{:02}{:02}{:02}{:02}Z", m, d, secs / 3600, secs / 60 % 60, secs % 60);
    if (1950..2050).contains(&y) {
        w.tlv(der::UTC_TIME, alloc::format!("{:02}{text}", y % 100).as_bytes());
    } else {
        w.tlv(der::GENERALIZED_TIME, alloc::format!("{y:04}{text}").as_bytes());
    }
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}
