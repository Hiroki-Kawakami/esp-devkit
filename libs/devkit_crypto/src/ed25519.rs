/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use crate::ct;
use crate::fe25519::{self as fe, Fe, add, from_bytes, invert, is_negative, mul, pow22523, square, sub, to_bytes};
use crate::hash::{Hash, Sha512};
use crate::wnaf::wnaf;
use crate::{Error, Result};

const FE0: Fe = fe::ZERO;
const FE1: Fe = fe::ONE;
const D: Fe = fe::from_words(&[
    0x78a3, 0x1359, 0x4dca, 0x75eb, 0xd8ab, 0x4141, 0x0a4d, 0x0070, 0xe898, 0x7779, 0x4079, 0x8cc7, 0xfe73, 0x2b6f, 0x6cee,
    0x5203,
]);
const D2: Fe = fe::from_words(&[
    0xf159, 0x26b2, 0x9b94, 0xebd6, 0xb156, 0x8283, 0x149a, 0x00e0, 0xd130, 0xeef3, 0x80f2, 0x198e, 0xfce7, 0x56df, 0xd9dc,
    0x2406,
]);
const BASE_X: Fe = fe::from_words(&[
    0xd51a, 0x8f25, 0x2d60, 0xc956, 0xa7b2, 0x9525, 0xc760, 0x692c, 0xdc5c, 0xfdd6, 0xe231, 0xc0a4, 0x53fe, 0xcd6e, 0x36d3,
    0x2169,
]);
const BASE_Y: Fe = fe::from_words(&[
    0x6658, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666,
    0x6666,
]);
const SQRT_M1: Fe = fe::from_words(&[
    0xa0b0, 0x4a0e, 0x1b27, 0xc4ee, 0xe478, 0xad2f, 0x1806, 0x2f43, 0xd7a7, 0x3dfb, 0x0099, 0x2b4d, 0xdf0b, 0x4fc1, 0x2480,
    0x2b83,
]);
const L: [i64; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0x10,
];

/// A point on edwards25519, in extended coordinates. Operations on it take the
/// same time for any input.
#[derive(Clone, Copy)]
pub struct Point([Fe; 4]);

impl Point {
    pub const IDENTITY: Point = Point([FE0, FE1, FE1, FE0]);

    pub fn base() -> Point {
        Point([BASE_X, BASE_Y, FE1, mul(&BASE_X, &BASE_Y)])
    }

    /// RFC 8032 decoding; `None` for a non-canonical `y`, `x = 0` with the sign
    /// bit set, or a `y` with no point.
    pub fn decode(bytes: &[u8; 32]) -> Option<Point> {
        let y = from_bytes(bytes);
        let mut canonical = to_bytes(&y);
        canonical[31] |= bytes[31] & 0x80;
        if canonical != *bytes {
            return None;
        }
        let y2 = square(&y);
        let num = sub(&y2, &FE1);
        let den = add(&mul(&y2, &D), &FE1);
        let den2 = square(&den);
        let den4 = square(&den2);
        let den6 = mul(&den4, &den2);
        let mut t = mul(&mul(&den6, &num), &den);
        t = pow22523(&t);
        t = mul(&mul(&mul(&t, &num), &den), &den);
        let mut x = mul(&t, &den);
        if !fe::eq(&mul(&square(&x), &den), &num) {
            x = mul(&x, &SQRT_M1);
        }
        if !fe::eq(&mul(&square(&x), &den), &num) {
            return None;
        }
        let sign = bytes[31] >> 7;
        if fe::eq(&x, &FE0) && sign == 1 {
            return None;
        }
        if is_negative(&x) != sign {
            x = sub(&FE0, &x);
        }
        Some(Point([x, y, FE1, mul(&x, &y)]))
    }

    pub fn encode(&self) -> [u8; 32] {
        let zi = invert(&self.0[2]);
        let x = mul(&self.0[0], &zi);
        let y = mul(&self.0[1], &zi);
        let mut out = to_bytes(&y);
        out[31] ^= is_negative(&x) << 7;
        out
    }

    pub fn add(&self, q: &Point) -> Point {
        let [p0, p1, p2, p3] = &self.0;
        let [q0, q1, q2, q3] = &q.0;
        let a = mul(&sub(p1, p0), &sub(q1, q0));
        let b = mul(&add(p0, p1), &add(q0, q1));
        let c = mul(&mul(p3, q3), &D2);
        let d = mul(p2, q2);
        let d = add(&d, &d);
        let e = sub(&b, &a);
        let f = sub(&d, &c);
        let g = add(&d, &c);
        let h = add(&b, &a);
        Point([mul(&e, &f), mul(&h, &g), mul(&g, &f), mul(&e, &h)])
    }

    fn double(&self) -> Point {
        let [x, y, z, _] = &self.0;
        let a = square(x);
        let b = square(y);
        let c = square(z);
        let c = add(&c, &c);
        let d = fe::neg(&a);
        let e = sub(&sub(&square(&add(x, y)), &a), &b);
        let g = add(&d, &b);
        let f = sub(&g, &c);
        let h = sub(&d, &b);
        Point([mul(&e, &f), mul(&g, &h), mul(&f, &g), mul(&e, &h)])
    }

    pub fn neg(&self) -> Point {
        let [x, y, z, t] = self.0;
        Point([sub(&FE0, &x), y, z, sub(&FE0, &t)])
    }

    pub fn sub(&self, q: &Point) -> Point {
        self.add(&q.neg())
    }

    /// By a little-endian 256-bit integer, used as it is; unlike [`Point::mul`]
    /// nothing is reduced modulo the group order.
    pub fn mul_bits(&self, scalar: &[u8; 32]) -> Point {
        let mut p = Point::IDENTITY;
        let mut q = *self;
        for i in (0..256).rev() {
            let bit = ((scalar[i / 8] >> (i & 7)) & 1) as i32;
            swap(&mut p, &mut q, bit);
            q = q.add(&p);
            p = p.double();
            swap(&mut p, &mut q, bit);
        }
        p
    }

    pub fn mul(&self, scalar: &Scalar) -> Point {
        self.mul_bits(&scalar.0)
    }

    pub fn mul_base(scalar: &Scalar) -> Point {
        Point::base().mul(scalar)
    }

    /// `a·B + b·self` in variable time, for public scalars only.
    #[inline(never)]
    fn mul2_vartime(&self, a: &Scalar, b: &Scalar) -> Point {
        fn odd_multiples(p: &Point) -> Vec<Point> {
            let p2 = p.double();
            let mut t = Vec::with_capacity(8);
            t.push(*p);
            for i in 1..8 {
                t.push(t[i - 1].add(&p2));
            }
            t
        }
        let limbs = |s: &Scalar| -> [u32; 8] { core::array::from_fn(|i| u32::from_le_bytes(s.0[4 * i..4 * i + 4].try_into().unwrap())) };
        let (na, nb) = (wnaf(&limbs(a), 5), wnaf(&limbs(b), 5));
        let (ta, tb) = (odd_multiples(&Point::base()), odd_multiples(self));
        let mut r = Point::IDENTITY;
        for i in (0..na.len()).rev() {
            r = r.double();
            for (d, t) in [(na[i], &ta), (nb[i], &tb)] {
                if d > 0 {
                    r = r.add(&t[(d / 2) as usize]);
                } else if d < 0 {
                    r = r.sub(&t[(-d / 2) as usize]);
                }
            }
        }
        r
    }
}

impl PartialEq for Point {
    fn eq(&self, other: &Point) -> bool {
        ct::eq(&self.encode(), &other.encode())
    }
}

impl Eq for Point {}

impl core::fmt::Debug for Point {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Point({:02x?})", self.encode())
    }
}

fn swap(p: &mut Point, q: &mut Point, bit: i32) {
    let (p, q) = (&mut p.0, &mut q.0);
    for i in 0..4 {
        fe::cswap(&mut p[i], &mut q[i], bit);
    }
}

fn reduce(x: &mut [i64; 64]) -> [u8; 32] {
    for i in (32..64).rev() {
        let mut carry = 0;
        let mut j = i - 32;
        while j < i - 12 {
            x[j] += carry - 16 * x[i] * L[j - (i - 32)];
            carry = (x[j] + 128) >> 8;
            x[j] -= carry << 8;
            j += 1;
        }
        x[j] += carry;
        x[i] = 0;
    }
    let mut carry = 0;
    for j in 0..32 {
        x[j] += carry - (x[31] >> 4) * L[j];
        carry = x[j] >> 8;
        x[j] &= 255;
    }
    for j in 0..32 {
        x[j] -= carry * L[j];
    }
    let mut r = [0u8; 32];
    for i in 0..32 {
        x[i + 1] += x[i] >> 8;
        r[i] = (x[i] & 255) as u8;
    }
    r
}

/// An integer modulo the group order ℓ, little-endian.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scalar([u8; 32]);

impl Scalar {
    pub const ZERO: Scalar = Scalar([0; 32]);

    pub fn from_bytes_mod_order(bytes: &[u8; 32]) -> Scalar {
        let mut x = [0i64; 64];
        for (x, &b) in x.iter_mut().zip(bytes) {
            *x = b as i64;
        }
        Scalar(reduce(&mut x))
    }

    pub fn from_bytes_mod_order_wide(bytes: &[u8; 64]) -> Scalar {
        let mut x = [0i64; 64];
        for (x, &b) in x.iter_mut().zip(bytes) {
            *x = b as i64;
        }
        Scalar(reduce(&mut x))
    }

    /// `None` unless the value is already below ℓ.
    pub fn from_canonical_bytes(bytes: &[u8; 32]) -> Option<Scalar> {
        let s = Scalar::from_bytes_mod_order(bytes);
        (s.0 == *bytes).then_some(s)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    pub fn add(&self, other: &Scalar) -> Scalar {
        let mut x = [0i64; 64];
        for i in 0..32 {
            x[i] = self.0[i] as i64 + other.0[i] as i64;
        }
        Scalar(reduce(&mut x))
    }

    pub fn mul(&self, other: &Scalar) -> Scalar {
        mul_add(&self.0, &other.0, &Scalar::ZERO)
    }

    pub fn neg(&self) -> Scalar {
        let mut out = [0u8; 32];
        let mut borrow = 0i64;
        for i in 0..32 {
            let v = L[i] - self.0[i] as i64 - borrow;
            borrow = (v < 0) as i64;
            out[i] = (v + (borrow << 8)) as u8;
        }
        Scalar::from_bytes_mod_order(&out)
    }
}

fn mul_add(a: &[u8; 32], b: &[u8; 32], c: &Scalar) -> Scalar {
    let mut x = [0i64; 64];
    for i in 0..32 {
        x[i] = c.0[i] as i64;
    }
    for i in 0..32 {
        for j in 0..32 {
            x[i + j] += a[i] as i64 * b[j] as i64;
        }
    }
    Scalar(reduce(&mut x))
}

fn hash_to_scalar(parts: &[&[u8]]) -> Scalar {
    let mut h = Sha512::new();
    for part in parts {
        h.update(part);
    }
    Scalar::from_bytes_mod_order_wide(&h.finalize())
}

/// An Ed25519 (RFC 8032) private key.
pub struct SigningKey {
    seed: [u8; 32],
    scalar: [u8; 32],
    prefix: [u8; 32],
    public: [u8; 32],
}

impl SigningKey {
    pub fn from_seed(seed: &[u8; 32]) -> SigningKey {
        let h = Sha512::digest(seed);
        let mut scalar: [u8; 32] = h[..32].try_into().unwrap();
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;
        let public = Point::base().mul_bits(&scalar).encode();
        SigningKey { seed: *seed, scalar, prefix: h[32..].try_into().unwrap(), public }
    }

    pub fn generate() -> SigningKey {
        let mut seed = [0; 32];
        crate::rand::fill(&mut seed);
        let key = SigningKey::from_seed(&seed);
        ct::zeroize(&mut seed);
        key
    }

    pub fn seed(&self) -> &[u8; 32] {
        &self.seed
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey::from_bytes(&self.public).expect("derived from a valid scalar")
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let r = hash_to_scalar(&[&self.prefix, message]);
        let big_r = Point::mul_base(&r).encode();
        let k = hash_to_scalar(&[&big_r, &self.public, message]);
        let s = mul_add(&k.0, &self.scalar, &r);
        let mut sig = [0; 64];
        sig[..32].copy_from_slice(&big_r);
        sig[32..].copy_from_slice(&s.0);
        sig
    }
}

impl Drop for SigningKey {
    fn drop(&mut self) {
        ct::zeroize(&mut self.seed);
        ct::zeroize(&mut self.scalar);
        ct::zeroize(&mut self.prefix);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VerifyingKey {
    bytes: [u8; 32],
}

impl VerifyingKey {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<VerifyingKey> {
        Point::decode(bytes).ok_or(Error::InvalidKey)?;
        Ok(VerifyingKey { bytes: *bytes })
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.bytes
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        let sig: &[u8; 64] = signature.try_into().map_err(|_| Error::Verify)?;
        let r: [u8; 32] = sig[..32].try_into().unwrap();
        let s = Scalar::from_canonical_bytes(&sig[32..].try_into().unwrap()).ok_or(Error::Verify)?;
        let k = hash_to_scalar(&[&r, &self.bytes, message]);
        let point = Point::decode(&self.bytes).ok_or(Error::Verify)?;
        let check = point.neg().mul2_vartime(&s, &k).encode();
        if ct::eq(&check, &r) { Ok(()) } else { Err(Error::Verify) }
    }
}
