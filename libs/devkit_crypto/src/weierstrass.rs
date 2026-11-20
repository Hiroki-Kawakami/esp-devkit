/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! ECDSA verification on P-256 and P-384 (y² = x³ − 3x + b), Montgomery
//! arithmetic over 32-bit limbs. Variable time: only public values pass through.

use alloc::vec::Vec;

use crate::ecdsa::Curve;
use crate::wnaf::wnaf;

struct Params {
    p: &'static [u8],
    n: &'static [u8],
    b: &'static [u8],
    gx: &'static [u8],
    gy: &'static [u8],
}

const P256: Params = Params {
    p: &hex32("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff"),
    n: &hex32("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"),
    b: &hex32("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b"),
    gx: &hex32("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"),
    gy: &hex32("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"),
};

const P384: Params = Params {
    p: &hex48("fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff"),
    n: &hex48("ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973"),
    b: &hex48("b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef"),
    gx: &hex48("aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7"),
    gy: &hex48("3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f"),
};

const fn hex<const L: usize>(s: &str) -> [u8; L] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            _ => c - b'a' + 10,
        }
    }
    let s = s.as_bytes();
    let mut out = [0; L];
    let mut i = 0;
    while i < L {
        out[i] = (nibble(s[2 * i]) << 4) | nibble(s[2 * i + 1]);
        i += 1;
    }
    out
}

const fn hex32(s: &str) -> [u8; 32] {
    hex(s)
}

const fn hex48(s: &str) -> [u8; 48] {
    hex(s)
}

/// An uncompressed SEC 1 point on `curve`.
pub(crate) fn on_curve(curve: Curve, sec1: &[u8]) -> bool {
    match curve {
        Curve::P256 => Ctx::<8>::new(&P256).point(sec1).is_some(),
        Curve::P384 => Ctx::<12>::new(&P384).point(sec1).is_some(),
        Curve::P521 => false,
    }
}

/// `r` and `s` big-endian, `scalar_len()` bytes each; `digest` is truncated
/// to the order's length.
pub(crate) fn verify(curve: Curve, sec1: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
    match curve {
        Curve::P256 => Ctx::<8>::new(&P256).verify(sec1, digest, r, s),
        Curve::P384 => Ctx::<12>::new(&P384).verify(sec1, digest, r, s),
        Curve::P521 => false,
    }
}

type Limbs<const N: usize> = [u32; N];

fn limbs<const N: usize>(be: &[u8]) -> Limbs<N> {
    let mut out = [0; N];
    for (i, &b) in be.iter().rev().enumerate() {
        out[i / 4] |= (b as u32) << (8 * (i % 4));
    }
    out
}

fn is_zero<const N: usize>(a: &Limbs<N>) -> bool {
    a.iter().all(|&w| w == 0)
}

fn less<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> bool {
    for i in (0..N).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

fn add_raw<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> (Limbs<N>, bool) {
    let mut out = [0; N];
    let mut carry = 0u64;
    for i in 0..N {
        let s = a[i] as u64 + b[i] as u64 + carry;
        out[i] = s as u32;
        carry = s >> 32;
    }
    (out, carry != 0)
}

fn sub_raw<const N: usize>(a: &Limbs<N>, b: &Limbs<N>) -> (Limbs<N>, bool) {
    let mut out = [0; N];
    let mut borrow = 0u64;
    for i in 0..N {
        let d = (a[i] as u64).wrapping_sub(b[i] as u64 + borrow);
        out[i] = d as u32;
        borrow = d >> 63;
    }
    (out, borrow != 0)
}

/// Arithmetic modulo an odd `m` above 2^(32N − 1), on values in Montgomery
/// form (times 2^(32N)) unless a function says otherwise.
struct Modulus<const N: usize> {
    m: Limbs<N>,
    inv: u32,
    one: Limbs<N>,
    r2: Limbs<N>,
}

impl<const N: usize> Modulus<N> {
    fn new(m: Limbs<N>) -> Self {
        let mut inv: u32 = 1;
        for _ in 0..5 {
            inv = inv.wrapping_mul(2u32.wrapping_sub(m[0].wrapping_mul(inv)));
        }
        let one = sub_raw(&[0; N], &m).0;
        let mut this = Self { m, inv: inv.wrapping_neg(), one, r2: one };
        for _ in 0..32 * N {
            this.r2 = this.add(&this.r2, &this.r2);
        }
        this
    }

    fn add(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let (s, carry) = add_raw(a, b);
        if carry || !less(&s, &self.m) { sub_raw(&s, &self.m).0 } else { s }
    }

    fn sub(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let (d, borrow) = sub_raw(a, b);
        if borrow { add_raw(&d, &self.m).0 } else { d }
    }

    /// `a · b / 2^(32N)`, so plain times Montgomery gives plain.
    fn mul(&self, a: &Limbs<N>, b: &Limbs<N>) -> Limbs<N> {
        let mut t = [0u32; N];
        let mut hi = 0u32;
        for &bi in b {
            let bi = bi as u64;
            let mut c = 0u64;
            for j in 0..N {
                let s = t[j] as u64 + a[j] as u64 * bi + c;
                t[j] = s as u32;
                c = s >> 32;
            }
            let s = hi as u64 + c;
            let (top, top2) = (s as u32, (s >> 32) as u32);
            let q = t[0].wrapping_mul(self.inv) as u64;
            let mut c = (t[0] as u64 + q * self.m[0] as u64) >> 32;
            for j in 1..N {
                let s = t[j] as u64 + q * self.m[j] as u64 + c;
                t[j - 1] = s as u32;
                c = s >> 32;
            }
            let s = top as u64 + c;
            t[N - 1] = s as u32;
            hi = top2 + (s >> 32) as u32;
        }
        if hi != 0 || !less(&t, &self.m) { sub_raw(&t, &self.m).0 } else { t }
    }

    fn sqr(&self, a: &Limbs<N>) -> Limbs<N> {
        self.mul(a, a)
    }

    /// From plain, which must be below `m`.
    fn to_mont(&self, a: &Limbs<N>) -> Limbs<N> {
        self.mul(a, &self.r2)
    }

    fn pow(&self, a: &Limbs<N>, e: &Limbs<N>) -> Limbs<N> {
        let mut r = self.one;
        for i in (0..32 * N).rev() {
            r = self.sqr(&r);
            if (e[i / 32] >> (i % 32)) & 1 == 1 {
                r = self.mul(&r, a);
            }
        }
        r
    }

    /// `m` must be prime.
    fn invert(&self, a: &Limbs<N>) -> Limbs<N> {
        let mut two = [0; N];
        two[0] = 2;
        self.pow(a, &sub_raw(&self.m, &two).0)
    }
}

/// Jacobian; `z == 0` is the point at infinity.
#[derive(Clone, Copy)]
struct Jac<const N: usize> {
    x: Limbs<N>,
    y: Limbs<N>,
    z: Limbs<N>,
}

struct Ctx<const N: usize> {
    f: Modulus<N>,
    n: Modulus<N>,
    b: Limbs<N>,
    g: Jac<N>,
    len: usize,
}

impl<const N: usize> Ctx<N> {
    fn new(params: &Params) -> Self {
        let f = Modulus::new(limbs(params.p));
        let n = Modulus::new(limbs(params.n));
        let b = f.to_mont(&limbs(params.b));
        let g = Jac { x: f.to_mont(&limbs(params.gx)), y: f.to_mont(&limbs(params.gy)), z: f.one };
        Self { f, n, b, g, len: params.p.len() }
    }

    fn point(&self, sec1: &[u8]) -> Option<Jac<N>> {
        if sec1.len() != 1 + 2 * self.len || sec1[0] != 4 {
            return None;
        }
        let (x, y) = (limbs(&sec1[1..1 + self.len]), limbs(&sec1[1 + self.len..]));
        if !less(&x, &self.f.m) || !less(&y, &self.f.m) {
            return None;
        }
        let f = &self.f;
        let (x, y) = (f.to_mont(&x), f.to_mont(&y));
        let x3 = f.mul(&f.sqr(&x), &x);
        let three_x = f.add(&f.add(&x, &x), &x);
        let rhs = f.add(&f.sub(&x3, &three_x), &self.b);
        (f.sqr(&y) == rhs).then_some(Jac { x, y, z: f.one })
    }

    fn double(&self, p: &Jac<N>) -> Jac<N> {
        if is_zero(&p.z) {
            return *p;
        }
        let f = &self.f;
        let delta = f.sqr(&p.z);
        let gamma = f.sqr(&p.y);
        let beta = f.mul(&p.x, &gamma);
        let alpha = f.mul(&f.sub(&p.x, &delta), &f.add(&p.x, &delta));
        let alpha = f.add(&f.add(&alpha, &alpha), &alpha);
        let beta2 = f.add(&beta, &beta);
        let beta4 = f.add(&beta2, &beta2);
        let x = f.sub(&f.sqr(&alpha), &f.add(&beta4, &beta4));
        let z = f.sub(&f.sub(&f.sqr(&f.add(&p.y, &p.z)), &gamma), &delta);
        let gamma2 = f.sqr(&gamma);
        let gamma2 = f.add(&gamma2, &gamma2);
        let gamma4 = f.add(&gamma2, &gamma2);
        let y = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x)), &f.add(&gamma4, &gamma4));
        Jac { x, y, z }
    }

    fn add(&self, p: &Jac<N>, q: &Jac<N>) -> Jac<N> {
        if is_zero(&p.z) {
            return *q;
        }
        if is_zero(&q.z) {
            return *p;
        }
        let f = &self.f;
        let z1z1 = f.sqr(&p.z);
        let z2z2 = f.sqr(&q.z);
        let u1 = f.mul(&p.x, &z2z2);
        let u2 = f.mul(&q.x, &z1z1);
        let s1 = f.mul(&f.mul(&p.y, &q.z), &z2z2);
        let s2 = f.mul(&f.mul(&q.y, &p.z), &z1z1);
        let h = f.sub(&u2, &u1);
        let r = f.sub(&s2, &s1);
        if is_zero(&h) {
            return if is_zero(&r) { self.double(p) } else { Jac { x: f.one, y: f.one, z: [0; N] } };
        }
        let hh = f.sqr(&h);
        let hhh = f.mul(&h, &hh);
        let v = f.mul(&u1, &hh);
        let x = f.sub(&f.sub(&f.sqr(&r), &hhh), &f.add(&v, &v));
        let y = f.sub(&f.mul(&r, &f.sub(&v, &x)), &f.mul(&s1, &hhh));
        let z = f.mul(&f.mul(&p.z, &q.z), &h);
        Jac { x, y, z }
    }

    fn neg(&self, p: &Jac<N>) -> Jac<N> {
        Jac { x: p.x, y: self.f.sub(&[0; N], &p.y), z: p.z }
    }

    /// `P, 3P, …, 15P`, for width-5 NAF digits.
    fn odd_multiples(&self, p: &Jac<N>) -> Vec<Jac<N>> {
        let p2 = self.double(p);
        let mut t = Vec::with_capacity(8);
        t.push(*p);
        for i in 1..8 {
            t.push(self.add(&t[i - 1], &p2));
        }
        t
    }

    /// `a·G + b·Q`, both scalars plain.
    #[inline(never)]
    fn mul2(&self, a: &Limbs<N>, b: &Limbs<N>, q: &Jac<N>) -> Jac<N> {
        let (na, nb) = (wnaf(a, 5), wnaf(b, 5));
        let (tg, tq) = (self.odd_multiples(&self.g), self.odd_multiples(q));
        let mut r = Jac { x: self.f.one, y: self.f.one, z: [0; N] };
        for i in (0..na.len()).rev() {
            r = self.double(&r);
            for (d, t) in [(na[i], &tg), (nb[i], &tq)] {
                if d > 0 {
                    r = self.add(&r, &t[(d / 2) as usize]);
                } else if d < 0 {
                    r = self.add(&r, &self.neg(&t[(-d / 2) as usize]));
                }
            }
        }
        r
    }

    // A TLS handshake runs on a task stack: inlined, both curves' frames and
    // the tables would land in the caller's.
    #[inline(never)]
    fn verify(&self, sec1: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
        let Some(q) = self.point(sec1) else { return false };
        let (r, s) = (limbs::<N>(r), limbs::<N>(s));
        if is_zero(&r) || is_zero(&s) || !less(&r, &self.n.m) || !less(&s, &self.n.m) {
            return false;
        }
        let mut e = limbs::<N>(&digest[..digest.len().min(self.len)]);
        if !less(&e, &self.n.m) {
            e = sub_raw(&e, &self.n.m).0;
        }
        let w = self.n.invert(&self.n.to_mont(&s));
        let (u1, u2) = (self.n.mul(&e, &w), self.n.mul(&r, &w));
        let point = self.mul2(&u1, &u2, &q);
        if is_zero(&point.z) {
            return false;
        }
        let f = &self.f;
        let zz = f.sqr(&point.z);
        if f.mul(&f.to_mont(&r), &zz) == point.x {
            return true;
        }
        let (rn, carry) = add_raw(&r, &self.n.m);
        !carry && less(&rn, &f.m) && f.mul(&f.to_mont(&rn), &zz) == point.x
    }
}
