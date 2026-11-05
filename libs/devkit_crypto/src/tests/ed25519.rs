/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::Error;
use crate::ed25519::{Point, Scalar, SigningKey, VerifyingKey};

use super::{hex, noise, num, vectors};

fn arr<const N: usize>(s: &str) -> [u8; N] {
    hex(s).try_into().unwrap()
}

// RFC 8032 section 7.1, tests 1 to 3.
const RFC8032: [(&str, &str, &str, &str); 3] = [
    (
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ),
    (
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ),
    (
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        "af82",
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    ),
];

#[test]
fn signatures_match_rfc8032() {
    for (seed, public, message, signature) in RFC8032 {
        let key = SigningKey::from_seed(&arr(seed));
        assert_eq!(key.verifying_key().to_bytes(), arr::<32>(public));
        assert_eq!(key.sign(&hex(message)).to_vec(), hex(signature));
        VerifyingKey::from_bytes(&arr(public)).unwrap().verify(&hex(message), &hex(signature)).unwrap();
    }
}

#[test]
fn signatures_match_the_references() {
    for row in vectors("ed25519") {
        let key = SigningKey::from_seed(&arr(&row[0]));
        assert_eq!(key.verifying_key().to_bytes(), arr::<32>(&row[1]));
        let n = num(&row[2]);
        let message = noise(n, n as u32 + 8);
        assert_eq!(key.sign(&message).to_vec(), hex(&row[3]));
    }
}

#[test]
fn bad_signatures_fail() {
    let key = SigningKey::from_seed(&[7; 32]);
    let public = key.verifying_key();
    let mut sig = key.sign(b"message");
    public.verify(b"message", &sig).unwrap();
    assert_eq!(public.verify(b"messagf", &sig).err(), Some(Error::Verify));
    assert_eq!(public.verify(b"message", &sig[..63]).err(), Some(Error::Verify));
    sig[63] |= 0xf0;
    assert_eq!(public.verify(b"message", &sig).err(), Some(Error::Verify));
}

#[test]
fn group_arithmetic_holds() {
    let base = Point::base();
    assert_eq!(base.encode(), arr::<32>("5866666666666666666666666666666666666666666666666666666666666666"));
    assert_eq!(Point::decode(&base.encode()), Some(base));
    let a = Scalar::from_bytes_mod_order(&arr("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"));
    let b = Scalar::from_bytes_mod_order_wide(&noise(64, 9).try_into().unwrap());
    assert_eq!(Point::mul_base(&a).add(&Point::mul_base(&b)), Point::mul_base(&a.add(&b)));
    assert_eq!(Point::mul_base(&a).mul(&b), Point::mul_base(&a.mul(&b)));
    assert_eq!(Point::mul_base(&a).sub(&Point::mul_base(&a)), Point::IDENTITY);
    assert_eq!(a.add(&a.neg()), Scalar::ZERO);
    assert_eq!(Point::mul_base(&a.neg()), Point::mul_base(&a).neg());
    let order = arr::<32>("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
    assert_eq!(base.mul_bits(&order), Point::IDENTITY);
    assert_eq!(Scalar::from_canonical_bytes(&order), None);
    let mut plus_order = a.to_bytes();
    let mut carry = 0u16;
    for (x, l) in plus_order.iter_mut().zip(order) {
        let sum = *x as u16 + l as u16 + carry;
        *x = sum as u8;
        carry = sum >> 8;
    }
    assert_eq!(base.mul_bits(&plus_order), Point::mul_base(&a));
}

#[test]
fn decoding_rejects_bad_encodings() {
    let mut p_bytes = [0xff; 32];
    p_bytes[0] = 0xed;
    p_bytes[31] = 0x7f;
    assert_eq!(Point::decode(&p_bytes), None);
    let mut identity_negative_x = Point::IDENTITY.encode();
    identity_negative_x[31] |= 0x80;
    assert_eq!(Point::decode(&identity_negative_x), None);
    let mut no_point = [0u8; 32];
    no_point[0] = 2;
    assert_eq!(Point::decode(&no_point), None);
}
