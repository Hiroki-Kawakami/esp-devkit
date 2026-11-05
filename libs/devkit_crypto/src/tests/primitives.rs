/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::Error;
use crate::cipher::chacha20;
use crate::hash::{Blake2s, Hash, Sha1, Sha256, Sha384, Sha512};
use crate::kdf::Hkdf;
use crate::mac::{Hmac, siphash};

use super::{hex, noise, num, vectors};

fn chunked<H: Hash>(data: &[u8]) -> H::Output {
    let mut hash = H::new();
    let mut rest = data;
    let mut step = 1;
    while !rest.is_empty() {
        let n = step.min(rest.len());
        hash.update(&rest[..n]);
        rest = &rest[n..];
        step = step * 3 + 1;
    }
    hash.finalize()
}

fn by_name<R>(name: &str, f: &mut dyn FnMut(&dyn Fn(&[u8]) -> Vec<u8>, &dyn Fn(&[u8], &[u8]) -> Vec<u8>) -> R) -> R {
    macro_rules! with {
        ($h:ty) => {
            f(&|d| chunked::<$h>(d).to_vec(), &|k, d| Hmac::<$h>::mac(k, d).to_vec())
        };
    }
    match name {
        "sha1" => with!(Sha1),
        "sha256" => with!(Sha256),
        "sha384" => with!(Sha384),
        "sha512" => with!(Sha512),
        "blake2s" => with!(Blake2s),
        _ => unreachable!(),
    }
}

fn hkdf(name: &str, salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> (Vec<u8>, Vec<u8>) {
    fn run<H: Hash>(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> (Vec<u8>, Vec<u8>) {
        let prk = Hkdf::<H>::extract(salt, ikm);
        let mut okm = vec![0; len];
        let (head, tail) = info.split_at(info.len() / 2);
        Hkdf::<H>::from_prk(prk.as_ref()).expand(&[head, tail], &mut okm).unwrap();
        (prk.as_ref().to_vec(), okm)
    }
    match name {
        "sha1" => run::<Sha1>(salt, ikm, info, len),
        "sha256" => run::<Sha256>(salt, ikm, info, len),
        "sha384" => run::<Sha384>(salt, ikm, info, len),
        "sha512" => run::<Sha512>(salt, ikm, info, len),
        "blake2s" => run::<Blake2s>(salt, ikm, info, len),
        _ => unreachable!(),
    }
}

#[test]
fn hashes_match_the_references() {
    assert_eq!(Sha256::digest(b"abc").to_vec(), hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    assert_eq!(Blake2s::digest(b"abc").to_vec(), hex("508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982"));
    for row in vectors("hash") {
        let n = num(&row[1]);
        let got = by_name(&row[0], &mut |hash, _| hash(&noise(n, n as u32)));
        assert_eq!(got, hex(&row[2]), "{} of {} bytes", row[0], n);
    }
}

#[test]
fn hash_states_clone() {
    let mut a = Sha384::new();
    a.update(b"common prefix ");
    let mut b = a.clone();
    a.update(b"one");
    b.update(b"two");
    assert_eq!(a.finalize(), Sha384::digest(b"common prefix one"));
    assert_eq!(b.finalize(), Sha384::digest(b"common prefix two"));
}

#[test]
fn hmac_matches_the_references() {
    let tag = Hmac::<Sha256>::mac(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(tag.to_vec(), hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"));
    for row in vectors("hmac") {
        let (k, n) = (num(&row[1]), num(&row[2]));
        let got = by_name(&row[0], &mut |_, mac| mac(&noise(k, 7 + k as u32), &noise(n, n as u32)));
        assert_eq!(got, hex(&row[3]), "{} with a {} byte key over {} bytes", row[0], k, n);
    }
}

#[test]
fn hmac_verifies_full_and_truncated_tags() {
    let tag = Hmac::<Sha256>::mac(b"key", b"data");
    let mac = || {
        let mut mac = Hmac::<Sha256>::new(b"key");
        mac.update(b"data");
        mac
    };
    assert!(mac().verify(&tag));
    assert!(mac().verify(&tag[..16]));
    let mut bad = tag;
    bad[31] ^= 1;
    assert!(!mac().verify(&bad));
    assert!(!mac().verify(&[]));
}

#[test]
fn hkdf_matches_the_references() {
    let ikm = [0x0b; 22];
    let salt: Vec<u8> = (0..13).collect();
    let info: Vec<u8> = (0xf0..=0xf9).collect();
    let prk = Hkdf::<Sha256>::extract(&salt, &ikm);
    assert_eq!(prk.to_vec(), hex("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"));
    let mut okm = [0; 42];
    Hkdf::<Sha256>::new(&salt, &ikm).expand(&[&info], &mut okm).unwrap();
    assert_eq!(okm.to_vec(), hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"));
    for row in vectors("hkdf") {
        let [salt, ikm, info, len] = [1, 2, 3, 4].map(|i| num(&row[i]));
        let (prk, okm) = hkdf(&row[0], &noise(salt, 1), &noise(ikm, 2), &noise(info, 3), len);
        assert_eq!(prk, hex(&row[5]), "{} prk", row[0]);
        assert_eq!(okm, hex(&row[6]), "{} okm of {}", row[0], len);
    }
}

#[test]
fn hkdf_rejects_too_long_output() {
    let mut out = vec![0; 255 * 32 + 1];
    assert_eq!(Hkdf::<Sha256>::new(b"", b"ikm").expand(&[], &mut out), Err(Error::InvalidLength));
}

#[test]
fn keyed_blake2s_matches_the_references() {
    for row in vectors("blake2s_keyed") {
        let [k, size, n] = [0, 1, 2].map(|i| num(&row[i]));
        let mut hash = Blake2s::with_key(&noise(k, 7 + k as u32), size).unwrap();
        hash.update(&noise(n, n as u32));
        let mut out = vec![0; size];
        hash.finalize_into(&mut out);
        assert_eq!(out, hex(&row[3]), "key {k} size {size} over {n} bytes");
    }
    assert_eq!(Blake2s::with_key(&[0; 33], 32).err(), Some(Error::InvalidLength));
    assert_eq!(Blake2s::with_key(&[], 0).err(), Some(Error::InvalidLength));
}

#[test]
fn chacha20_matches_rfc8439() {
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let nonce = [0, 0, 0, 0, 0, 0, 0, 0x4a, 0, 0, 0, 0];
    let mut data = *b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    chacha20(&key, &nonce, 1, &mut data);
    assert_eq!(data[..16], [0x6e, 0x2e, 0x35, 0x9a, 0x25, 0x68, 0xf9, 0x80, 0x41, 0xba, 0x07, 0x28, 0xdd, 0x0d, 0x69, 0x81]);
    assert_eq!(data[data.len() - 4..], [0x5e, 0x42, 0x87, 0x4d]);
    chacha20(&key, &nonce, 1, &mut data);
    assert!(data.starts_with(b"Ladies"));
}

#[test]
fn siphash_matches_the_reference() {
    let message: [u8; 15] = core::array::from_fn(|i| i as u8);
    assert_eq!(siphash(0x0706050403020100, 0x0f0e0d0c0b0a0908, &message), 0xa129ca6149be45e5);
}

#[test]
fn random_bytes_differ() {
    let mut a = [0; 32];
    let mut b = [0; 32];
    crate::rand::fill(&mut a);
    crate::rand::fill(&mut b);
    assert_ne!(a, b);
}
