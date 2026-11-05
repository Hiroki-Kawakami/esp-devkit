/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::Error;
use crate::aead::{Aead, Algorithm, TAG};
use crate::ecdh::{EphemeralSecret, Group};
use crate::ecdsa::{self, Curve, SigningKey, VerifyingKey};
use crate::hash::HashAlg;
use crate::rsa::{PrivateKey, PublicKey};

use super::{hex, noise, num, vectors};

#[test]
fn aead_matches_the_references() {
    for row in vectors("aead") {
        let algorithm = match row[0].as_str() {
            "aes128gcm" => Algorithm::Aes128Gcm,
            "aes256gcm" => Algorithm::Aes256Gcm,
            _ => Algorithm::ChaCha20Poly1305,
        };
        let (n, aad_len) = (num(&row[1]), num(&row[2]));
        let key = noise(algorithm.key_len(), n as u32 + 1);
        let nonce: [u8; 12] = noise(12, n as u32 + 2).try_into().unwrap();
        let aad = noise(aad_len, n as u32 + 4);
        let aead = Aead::new(algorithm, &key).unwrap();
        let mut buf = noise(n, n as u32 + 3);
        buf.resize(n + TAG, 0);
        aead.seal_in_place(&nonce, &aad, &mut buf, n).unwrap();
        assert_eq!(buf, hex(&row[3]), "{} over {n} bytes", row[0]);
        let plain = aead.open_in_place(&nonce, &aad, &mut buf).unwrap();
        assert_eq!(plain, noise(n, n as u32 + 3));
        let mut tampered = hex(&row[3]);
        tampered[0] ^= 1;
        assert_eq!(aead.open_in_place(&nonce, &aad, &mut tampered).err(), Some(Error::Verify));
    }
    assert_eq!(Aead::new(Algorithm::Aes128Gcm, &[0; 32]).err(), Some(Error::InvalidLength));
}

#[test]
fn key_agreement_matches_the_references() {
    for row in vectors("x25519") {
        let secret = EphemeralSecret::from_private(Group::X25519, &hex(&row[0])).unwrap();
        assert_eq!(secret.agree(&hex(&row[1])).unwrap(), hex(&row[2]));
    }
    for row in vectors("ecdh") {
        let group = if row[0] == "p256" { Group::P256 } else { Group::P384 };
        let secret = EphemeralSecret::from_private(group, &hex(&row[1])).unwrap();
        assert_eq!(secret.agree(&hex(&row[2])).unwrap(), hex(&row[3]));
    }
}

#[test]
fn ephemeral_secrets_agree() {
    for group in [Group::X25519, Group::P256, Group::P384] {
        let a = EphemeralSecret::generate(group).unwrap();
        let b = EphemeralSecret::generate(group).unwrap();
        let a_pub = a.public_key().unwrap();
        assert_eq!(a_pub.len(), group.public_len());
        assert_eq!(a.agree(&b.public_key().unwrap()).unwrap(), b.agree(&a_pub).unwrap());
    }
    let p256 = EphemeralSecret::generate(Group::P256).unwrap();
    let mut off_curve = p256.public_key().unwrap();
    off_curve[64] ^= 1;
    assert_eq!(p256.agree(&off_curve).err(), Some(Error::InvalidKey));
}

#[test]
fn ecdsa_verifies_the_references_and_its_own() {
    for row in vectors("ecdsa") {
        let curve = match row[0].as_str() {
            "p256" => Curve::P256,
            "p384" => Curve::P384,
            _ => Curve::P521,
        };
        let key = VerifyingKey::from_sec1(curve, &hex(&row[1])).unwrap();
        let n = num(&row[2]);
        let message = noise(n, n as u32 + 5);
        let sig = hex(&row[3]);
        key.verify_der(curve.hash(), &message, &sig).unwrap();
        let raw = ecdsa::signature_from_der(curve, &sig).unwrap();
        assert_eq!(ecdsa::signature_to_der(&raw), sig);
        assert_eq!(key.verify_der(curve.hash(), b"other", &sig).err(), Some(Error::Verify));
    }
    for curve in [Curve::P256, Curve::P384, Curve::P521] {
        let key = SigningKey::generate(curve).unwrap();
        let sig = key.sign(curve.hash(), b"message").unwrap();
        assert_eq!(sig.len(), 2 * curve.scalar_len());
        let public = key.verifying_key().unwrap();
        public.verify(curve.hash(), b"message", &sig).unwrap();
        assert_eq!(public.verify(curve.hash(), b"messagf", &sig).err(), Some(Error::Verify));
        let again = SigningKey::from_bytes(curve, &key.to_bytes().unwrap()).unwrap();
        assert_eq!(again.verifying_key().unwrap().to_sec1(), public.to_sec1());
    }
}

#[test]
fn rsa_matches_the_references() {
    let key_row = &vectors("rsa_key")[0];
    let private = PrivateKey::from_pkcs1_der(&hex(&key_row[0])).unwrap();
    let public = PublicKey::from_pkcs1_der(&hex(&key_row[1])).unwrap();
    assert_eq!(private.public_key().unwrap().to_pkcs1_der(), public.to_pkcs1_der());
    assert_eq!(public.size(), 256);
    assert_eq!(public.exponent(), [1, 0, 1]);
    assert_eq!(private.to_pkcs1_der().unwrap(), hex(&key_row[0]));
    let message = noise(77, 6);
    for row in vectors("rsa_sig") {
        let hash = match row[0].as_str() {
            "sha256" => HashAlg::Sha256,
            "sha384" => HashAlg::Sha384,
            _ => HashAlg::Sha512,
        };
        let pkcs1 = hex(&row[1]);
        assert_eq!(private.sign_pkcs1v15(hash, &message).unwrap(), pkcs1);
        public.verify_pkcs1v15(hash, &message, &pkcs1).unwrap();
        public.verify_pss(hash, &message, &hex(&row[2])).unwrap();
        let ours = private.sign_pss(hash, &message).unwrap();
        public.verify_pss(hash, &message, &ours).unwrap();
        assert_eq!(public.verify_pss(hash, b"other", &ours).err(), Some(Error::Verify));
        assert_eq!(public.verify_pkcs1v15(hash, &message, &pkcs1[1..]).err(), Some(Error::Verify));
    }
}

#[test]
fn rsa_signs_prehashed_digests() {
    let private = PrivateKey::from_pkcs1_der(&hex(&vectors("rsa_key")[0][0])).unwrap();
    for row in vectors("rsa_prehashed") {
        let hash = if row[0] == "sha1" { HashAlg::Sha1 } else { HashAlg::Sha256 };
        let digest = noise(hash.output_len(), 9);
        assert_eq!(private.sign_pkcs1v15_prehashed(hash, &digest).unwrap(), hex(&row[1]), "{}", row[0]);
    }
    assert_eq!(private.sign_pkcs1v15_prehashed(HashAlg::Sha1, &[0; 32]).err(), Some(Error::InvalidLength));
}

#[test]
fn rsa_generates_keys() {
    let key = PrivateKey::generate(1024).unwrap();
    let public = key.public_key().unwrap();
    assert_eq!(public.size(), 128);
    let sig = key.sign_pss(HashAlg::Sha256, b"m").unwrap();
    public.verify_pss(HashAlg::Sha256, b"m", &sig).unwrap();
}
