/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::ecdsa::{Curve, SigningKey};
use crate::x509::{
    Certificate, CertificateBuilder, Name, PublicKey, ServerName, SignatureAlgorithm, Signer, TrustStore, verify_chain,
};
use crate::{CertError, Error, ed25519, rsa};
use crate::hash::HashAlg;

use super::fixture;

const NOW: Option<i64> = Some(1_800_000_000);

fn root_store() -> TrustStore {
    let mut store = TrustStore::new();
    store.add_der(&fixture("root.der")).unwrap();
    store
}

fn check(chain: &[&str], name: &str) -> Result<(), Error> {
    let ders: Vec<Vec<u8>> = chain.iter().map(|n| fixture(&format!("{n}.der"))).collect();
    let refs: Vec<&[u8]> = ders.iter().map(|d| d.as_slice()).collect();
    let leaf = Certificate::parse(refs[0]).unwrap();
    let now = Some((leaf.not_before + leaf.not_after) / 2);
    verify_chain(&refs, &root_store(), now, Some(&ServerName::parse(name)))
}

#[test]
fn parses_the_fields() {
    let der = fixture("leaf_rsa.der");
    let cert = Certificate::parse(&der).unwrap();
    assert_eq!(cert.signature_algorithm, SignatureAlgorithm::RsaPkcs1(HashAlg::Sha256));
    assert_eq!(cert.dns_names().collect::<Vec<_>>(), [b"localhost"]);
    assert_eq!(cert.ip_addresses().collect::<Vec<_>>(), [[127, 0, 0, 1]]);
    assert_eq!(cert.basic_constraints.map(|b| b.ca), Some(false));
    assert!(cert.not_after - cert.not_before > 364 * 86400);
    assert!(matches!(cert.public_key().unwrap(), PublicKey::Rsa(_)));
    let pss = fixture("leaf_pss.der");
    assert_eq!(Certificate::parse(&pss).unwrap().signature_algorithm, SignatureAlgorithm::RsaPss(HashAlg::Sha256));
    let ed = fixture("leaf_ed25519.der");
    assert!(matches!(Certificate::parse(&ed).unwrap().public_key().unwrap(), PublicKey::Ed25519(_)));
    assert_eq!(Certificate::parse(&der[..der.len() - 1]).err(), Some(Error::Decode));
}

#[test]
fn chains_verify_to_the_root() {
    check(&["leaf_rsa", "inter"], "localhost").unwrap();
    check(&["leaf_rsa", "inter"], "127.0.0.1").unwrap();
    check(&["leaf_pss", "inter"], "LOCALHOST.").unwrap();
    check(&["leaf_ec"], "localhost").unwrap();
    check(&["leaf_p384"], "localhost").unwrap();
    check(&["leaf_ed25519"], "localhost").unwrap();
    check(&["leaf_rsa", "inter_nc", "inter"], "localhost").unwrap();
}

#[test]
fn bad_chains_fail() {
    let fail = |e| Err(Error::Certificate(e));
    assert_eq!(check(&["leaf_rsa"], "localhost"), fail(CertError::UnknownIssuer));
    assert_eq!(check(&["leaf_rsa", "inter"], "example.com"), fail(CertError::NameMismatch));
    assert_eq!(check(&["leaf_rsa", "inter"], "10.0.0.1"), fail(CertError::NameMismatch));
    assert_eq!(check(&["leaf_nc", "inter_nc"], "localhost"), fail(CertError::NameConstraint));
    let der = [fixture("leaf_ec.der")];
    let refs: Vec<&[u8]> = der.iter().map(|d| d.as_slice()).collect();
    assert_eq!(verify_chain(&refs, &root_store(), Some(4_000_000_000), None), fail(CertError::Expired));
    assert_eq!(verify_chain(&refs, &root_store(), Some(0), None), fail(CertError::NotYetValid));
    verify_chain(&refs, &root_store(), None, None).unwrap();
    assert_eq!(verify_chain(&refs, &TrustStore::new(), NOW, None), fail(CertError::UnknownIssuer));
}

#[test]
fn a_trusted_leaf_needs_no_issuer() {
    let mut store = TrustStore::new();
    store.add_der(&fixture("leaf_ec.der")).unwrap();
    let der = fixture("leaf_ec.der");
    verify_chain(&[&der], &store, None, Some(&ServerName::Dns("localhost"))).unwrap();
}

#[test]
fn built_certificates_parse_and_verify() {
    let rsa = rsa::PrivateKey::generate(2048).unwrap();
    let ec = SigningKey::generate(Curve::P384).unwrap();
    let ed = ed25519::SigningKey::from_seed(&[3; 32]);
    for signer in [Signer::Rsa(&rsa), Signer::Ecdsa(&ec), Signer::Ed25519(&ed)] {
        let der = CertificateBuilder::new(Name::new().common_name("devkit").organization("test").country("JP"))
            .serial(&[0x80, 1])
            .validity(1_700_000_000, 2_600_000_000)
            .ca(true)
            .dns_name("device.local")
            .self_signed(signer)
            .unwrap();
        let cert = Certificate::parse(&der).unwrap();
        assert_eq!(cert.not_before, 1_700_000_000);
        assert_eq!(cert.not_after, 2_600_000_000);
        assert_eq!(cert.serial, [0, 0x80, 1]);
        assert_eq!(cert.issuer, cert.subject);
        assert_eq!(cert.basic_constraints.map(|b| b.ca), Some(true));
        assert_eq!(cert.dns_names().collect::<Vec<_>>(), [b"device.local"]);
        assert_eq!(cert.spki, signer.spki().unwrap().as_slice());
        cert.verify_signed_by(&cert.public_key().unwrap()).unwrap();
    }
}

#[test]
fn the_mozilla_bundle_parses() {
    let dir = std::env::var("DEVKIT_CA_BUNDLE").expect("run inside the dev shell");
    let data: &'static [u8] = Vec::leak(std::fs::read(format!("{dir}/ca_bundle.bin")).unwrap());
    let store = TrustStore::from_bundle(data).unwrap();
    assert!(store.len() > 100);
    assert!(store.bundle_version().unwrap() >= 3000);
    for anchor in store.anchors() {
        PublicKey::from_spki(anchor.spki).unwrap_or_else(|e| panic!("{e:?} in a root key"));
    }
    assert!(store.anchors().any(|a| a.distrust_after.is_some()));
    let mut truncated = data[..data.len() - 1].to_vec();
    assert_eq!(TrustStore::from_bundle(Vec::leak(core::mem::take(&mut truncated))).err(), Some(Error::Decode));
}
