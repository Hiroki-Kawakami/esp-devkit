/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit::{A16, Zeroable};
use idf::heap::{self, Caps};

use super::tables::*;
use crate::{HAVE_PIE, Result};

const G: usize = 18;

/// `a * c / 2` for a Q31 `c`.
#[inline(always)]
pub(super) fn mulh(a: i32, c: i32) -> i32 {
    ((i64::from(a) * i64::from(c)) >> 32) as i32
}

/// `a * c / 2` for a Q15 `c`, floored per product so the kernels can split `a`
/// into two 16-bit halves and still match.
#[inline(always)]
pub(super) fn mulh16(a: i32, c: i32) -> i32 {
    ((i64::from(a) * i64::from(c)) >> 16) as i32
}

/// `a * c` for a Q15 `c`, floored per product, `|a|` below 2^30.
#[inline(always)]
pub(super) fn mul15(a: i32, c: i32) -> i32 {
    mulh(a.wrapping_mul(2), c.wrapping_mul(65536))
}

/// The window kernels take each 32-bit sample as `hi * 65536 + lo` with `lo`
/// signed, sum `hi * c + ((lo * c) >> 16)` and need 16-byte aligned operands.
#[inline(always)]
pub(super) unsafe fn split(hi: *mut i16, lo: *mut i16, v: i32) {
    unsafe {
        *hi = (v.wrapping_add(0x8000) >> 16) as i16;
        *lo = v as i16;
    }
}

#[inline(always)]
fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

#[inline(always)]
fn sub(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b)
}

#[inline(always)]
fn shl(a: i32, n: u32) -> i32 {
    a.wrapping_shl(n)
}

#[inline(always)]
unsafe fn quarter(p: *mut i32, r: i32, i: i32) {
    unsafe {
        *p = r >> 2;
        *p.add(1) = i >> 2;
    }
}

#[inline(always)]
unsafe fn rotate_quarter(p: *mut i32, r: i32, i: i32, w: *const i32) {
    unsafe {
        let (w0, w1) = (*w, *w.add(1));
        *p = sub(mulh(r, w0), mulh(i, w1)) >> 1;
        *p.add(1) = add(mulh(r, w1), mulh(i, w0)) >> 1;
    }
}

unsafe fn fft64(z: *mut i32) {
    let tw = AAC_FFT_TWIDDLE31.as_ptr();
    let mut len = 64;
    while len >= 4 {
        let q = len >> 2;
        let step = 2 * (64 / len);
        for block in 0..64 / len {
            let b = block * len;
            for j in 0..q {
                unsafe {
                    let x0 = z.add(2 * (b + j));
                    let x1 = x0.add(2 * q);
                    let x2 = x1.add(2 * q);
                    let x3 = x2.add(2 * q);
                    let (ar, ai) = (add(*x0, *x2), add(*x0.add(1), *x2.add(1)));
                    let (br, bi) = (sub(*x0, *x2), sub(*x0.add(1), *x2.add(1)));
                    let (cr, ci) = (add(*x1, *x3), add(*x1.add(1), *x3.add(1)));
                    let (dr, di) = (sub(*x1, *x3), sub(*x1.add(1), *x3.add(1)));
                    quarter(x0, add(ar, cr), add(ai, ci));
                    if j != 0 {
                        rotate_quarter(x1, add(br, di), sub(bi, dr), tw.add(j * step));
                        rotate_quarter(x2, sub(ar, cr), sub(ai, ci), tw.add(2 * j * step));
                        rotate_quarter(x3, sub(br, di), add(bi, dr), tw.add(3 * j * step));
                    } else {
                        quarter(x1, add(br, di), sub(bi, dr));
                        quarter(x2, sub(ar, cr), sub(ai, ci));
                        quarter(x3, sub(br, di), add(bi, dr));
                    }
                }
            }
        }
        len = q;
    }
}

/// DCT-IV(128) in place scaled by 1/256; `tmp` holds 128 words.
pub(super) unsafe fn dct4_128(x: *mut i32, tmp: *mut i32) {
    unsafe {
        for n in 0..64 {
            let (vr, vi) = (*x.add(2 * n), *x.add(127 - 2 * n));
            let (wr, wi) = (AAC_DCT4_PRE31_128[2 * n], AAC_DCT4_PRE31_128[2 * n + 1]);
            *tmp.add(2 * n) = sub(mulh(vr, wr), mulh(vi, wi));
            *tmp.add(2 * n + 1) = add(mulh(vr, wi), mulh(vi, wr));
        }
        fft64(tmp);
        *x = *tmp >> 1;
        *x.add(127) = (*tmp.add(1) >> 1).wrapping_neg();
        for k in 1..64 {
            let t = tmp.add(2 * usize::from(AAC_FFT_POS_64[k]));
            let (wr, wi) = (AAC_DCT4_POST31_128[2 * k], AAC_DCT4_POST31_128[2 * k + 1]);
            *x.add(2 * k) = sub(mulh(*t, wr), mulh(*t.add(1), wi));
            *x.add(127 - 2 * k) = add(mulh(*t, wi), mulh(*t.add(1), wr)).wrapping_neg();
        }
    }
}

#[inline(always)]
fn split_mul(hi: i16, lo: i16, c: i16) -> i32 {
    add(i32::from(hi) * i32::from(c), (i32::from(lo) * i32::from(c)) >> 16)
}

/// `u[p]` = sum over `j < 5` of `x[p - 64 j] * c[64 j + p]`.
unsafe fn qmf_analysis_c(u: *mut i32, hi: *const i16, lo: *const i16, c: *const i16) {
    unsafe {
        for p in 0..64 {
            *u.add(p) = 0;
        }
        for j in 0..5 {
            let (h, l, cj) = (hi.sub(64 * j), lo.sub(64 * j), c.add(64 * j));
            for p in 0..64 {
                *u.add(p) = add(*u.add(p), split_mul(*h.add(p), *l.add(p), *cj.add(p)));
            }
        }
    }
}

/// `acc[k]` = sum over `t < 10` of `v[(t / 2) * 256 + (t % 2) * 192 + k] * c[64 t + k]`.
unsafe fn qmf_synthesis_c(acc: *mut i32, hi: *const i16, lo: *const i16, c: *const i16) {
    unsafe {
        for k in 0..64 {
            *acc.add(k) = 0;
        }
        for t in 0..10 {
            let off = (t >> 1) * 256 + (t & 1) * 192;
            let (h, l, ct) = (hi.add(off), lo.add(off), c.add(64 * t));
            for k in 0..64 {
                *acc.add(k) = add(*acc.add(k), split_mul(*h.add(k), *l.add(k), *ct.add(k)));
            }
        }
    }
}

#[inline(always)]
fn cmul15(ar: i32, ai: i32, w0: i16, w1: i16) -> (i32, i32) {
    let (w0, w1) = (i32::from(w0), i32::from(w1));
    (sub(mulh16(ar, w0), mulh16(ai, w1)), add(mulh16(ar, w1), mulh16(ai, w0)))
}

#[inline(always)]
fn bitrev5(k: usize) -> usize {
    (k & 1) << 4 | (k & 2) << 2 | (k & 4) | (k & 8) >> 2 | (k & 16) >> 4
}

/// DCT-IV(64) scaled by 1/128 of four interleaved transforms, `x[k * 4 + lane]`,
/// with Q15 twiddles; `tmp` holds 256 words.
unsafe fn dct4_64x4_c(x: *mut i32, tmp: *mut i32, _t: *const *const i16) {
    unsafe {
        let re = tmp;
        let im = tmp.add(128);
        for l in 0..4 {
            for n in 0..32 {
                let (r, i) = cmul15(
                    *x.add(2 * n * 4 + l),
                    *x.add((63 - 2 * n) * 4 + l),
                    AAC_Q15_DCT64_PRE[2 * n],
                    AAC_Q15_DCT64_PRE[2 * n + 1],
                );
                *re.add(n * 4 + l) = r;
                *im.add(n * 4 + l) = i;
            }
            let mut len = 32;
            while len >= 2 {
                let h = len >> 1;
                let step = 32 / len;
                for b in (0..32).step_by(len) {
                    for j in 0..h {
                        let (a, c) = ((b + j) * 4 + l, (b + j + h) * 4 + l);
                        let (ar, ai, cr, ci) = (*re.add(a), *im.add(a), *re.add(c), *im.add(c));
                        *re.add(a) = add(ar, cr) >> 1;
                        *im.add(a) = add(ai, ci) >> 1;
                        let (dr, di) = (sub(ar, cr), sub(ai, ci));
                        if j == 0 {
                            *re.add(c) = dr >> 1;
                            *im.add(c) = di >> 1;
                        } else if len == 4 {
                            *re.add(c) = di >> 1;
                            *im.add(c) = dr.wrapping_neg() >> 1;
                        } else {
                            let w = 2 * j * step;
                            let (r, i) = cmul15(dr, di, AAC_Q15_FFT32[w], AAC_Q15_FFT32[w + 1]);
                            *re.add(c) = r;
                            *im.add(c) = i;
                        }
                    }
                }
                len >>= 1;
            }
            for k in 0..32 {
                let p = bitrev5(k) * 4 + l;
                let (ur, ui) = if k != 0 {
                    cmul15(*re.add(p), *im.add(p), AAC_Q15_DCT64_POST[2 * k], AAC_Q15_DCT64_POST[2 * k + 1])
                } else {
                    (*re.add(p) >> 1, *im.add(p) >> 1)
                };
                *x.add(2 * k * 4 + l) = ur;
                *x.add((63 - 2 * k) * 4 + l) = ui.wrapping_neg();
            }
        }
    }
}

#[inline(always)]
fn cmul_lane(ar: i32, ai: i32, t: &[i16], idx: usize) -> (i32, i32) {
    cmul15(ar, ai, t[8 * (idx >> 2) + (idx & 3)], t[8 * (idx >> 2) + 4 + (idx & 3)])
}

/// DCT-IV(1024) `X` of `x` scaled by 1/2048 with Q15 twiddles, left as
/// `x[k] = X[2k]` and `x[512 + k] = X[1023 - 2k]`; `x` and `tmp` 16-byte aligned,
/// `tmp` holds 1024 words.
unsafe fn dct4_1024_c(x: *mut i32, tmp: *mut i32, _t: *const *const i16) {
    unsafe {
        let re = tmp;
        let im = tmp.add(512);
        for n in 0..512 {
            let (r, i) = cmul_lane(*x.add(2 * n), *x.add(1023 - 2 * n), &devkit_aac_pie_dct1024_pre.0, n);
            *re.add(n) = r;
            *im.add(n) = i;
        }
        for l in 0..4 {
            let mut len = 128;
            while len >= 2 {
                let h = len >> 1;
                let step = 128 / len;
                for b in (0..128).step_by(len) {
                    for j in 0..h {
                        let (a, c) = ((b + j) * 4 + l, (b + j + h) * 4 + l);
                        let (ar, ai, cr, ci) = (*re.add(a), *im.add(a), *re.add(c), *im.add(c));
                        *re.add(a) = add(ar, cr) >> 1;
                        *im.add(a) = add(ai, ci) >> 1;
                        let (dr, di) = (sub(ar, cr), sub(ai, ci));
                        if j == 0 {
                            *re.add(c) = dr >> 1;
                            *im.add(c) = di >> 1;
                        } else if len == 4 {
                            *re.add(c) = di >> 1;
                            *im.add(c) = dr.wrapping_neg() >> 1;
                        } else {
                            let (r, i) = cmul_lane(dr, di, &devkit_aac_pie_dct1024_fft128.0, 4 * j * step);
                            *re.add(c) = r;
                            *im.add(c) = i;
                        }
                    }
                }
                len >>= 1;
            }
        }
        const SLOT_OF: [usize; 4] = [0, 64, 32, 96];
        for p in 0..32 {
            for i in 0..4 {
                let at = (p + SLOT_OF[i]) * 4;
                let mut gr = [*re.add(at) >> 1, 0, 0, 0];
                let mut gi = [*im.add(at) >> 1, 0, 0, 0];
                for l in 1..4 {
                    let idx = 12 * p + 4 * (l - 1) + i;
                    (gr[l], gi[l]) = cmul_lane(*re.add(at + l), *im.add(at + l), &devkit_aac_pie_dct1024_mid.0, idx);
                }
                let (ar, ai) = (add(gr[0], gr[2]), add(gi[0], gi[2]));
                let (br, bi) = (sub(gr[0], gr[2]), sub(gi[0], gi[2]));
                let (cr, ci) = (add(gr[1], gr[3]), add(gi[1], gi[3]));
                let (dr, di) = (sub(gi[1], gi[3]), sub(gr[3], gr[1]));
                let zr = [add(ar, cr) >> 1, add(br, dr) >> 1, sub(ar, cr) >> 1, sub(br, dr) >> 1];
                let zi = [add(ai, ci) >> 1, add(bi, di) >> 1, sub(ai, ci) >> 1, sub(bi, di) >> 1];
                for q in 0..4 {
                    let k = (bitrev5(p) << 2) + i + 128 * q;
                    let (ur, ui) = if k != 0 {
                        cmul_lane(zr[q], zi[q], &devkit_aac_pie_dct1024_post.0, 4 * (p + 32 * q) + i)
                    } else {
                        (zr[q] >> 1, zi[q] >> 1)
                    };
                    *x.add(k) = ur;
                    *x.add(512 + k) = ui.wrapping_neg();
                }
            }
        }
    }
}

#[inline(always)]
fn cmul_vec(xr: i32, xi: i32, v0: i16, v4: i16) -> (i32, i32) {
    let (v0, v4) = (i32::from(v0), i32::from(v4));
    (sub(mulh16(xr, v0), mulh16(xi, v4)), add(mulh16(xr, v4), mulh16(xi, v0)))
}

#[inline(always)]
fn lane(k: usize, c: usize) -> usize {
    (k >> 2) * 8 + c * 4 + (k & 3)
}

/// A complex row of subbands is stored per group of four as `[re x4, im x4]`,
/// `k < 4 * groups`, and each product is `(x * c) >> 16`. Subbands 0-31 of the
/// delayed input `s` through the three-link all-pass `ap` (`[3 links][8 slots][64]`),
/// writing slot `w`, into `r`.
unsafe fn ps_allpass_c(r: *mut i32, s: *const i32, ap: *mut i32, w: i32) {
    let table = &devkit_aac_pie_ps_ap.0;
    unsafe {
        for k in 0..32 {
            let l = k & 3;
            let (re, im) = (lane(k, 0), lane(k, 1));
            let t = &table[56 * (k >> 2)..];
            let (mut ir, mut ii) = cmul_vec(*s.add(re), *s.add(im), t[l], t[4 + l]);
            ir = shl(ir, 1);
            ii = shl(ii, 1);
            for m in 0..3 {
                let q = &t[8 + 16 * m..];
                let link = ap.add((m * 8 + ((w - 3 - m as i32) & 7) as usize) * 64);
                let (cr, ci) = cmul_vec(*link.add(re), *link.add(im), q[l], q[4 + l]);
                let (q8, q12) = (i32::from(q[8 + l]), i32::from(q[12 + l]));
                let tr = shl(sub(cr, mulh16(ir, q8)), 1);
                let ti = shl(sub(ci, mulh16(ii, q12)), 1);
                let dst = ap.add((m * 8 + w as usize) * 64);
                *dst.add(re) = add(ir, shl(mulh16(tr, q8), 1));
                *dst.add(im) = add(ii, shl(mulh16(ti, q12), 1));
                ir = tr;
                ii = ti;
            }
            *r.add(re) = ir;
            *r.add(im) = ii;
        }
    }
}

/// `r *= 2 * g`, `g` as `[g x4, g x4]` Q15.
unsafe fn ps_gain_c(r: *mut i32, g: *const i16, groups: i32) {
    unsafe {
        for k in 0..4 * groups as usize {
            let v = g.add(8 * (k >> 2));
            let (re, im) = (lane(k, 0), lane(k, 1));
            *r.add(re) = shl(mulh16(*r.add(re), i32::from(*v.add(k & 3))), 1);
            *r.add(im) = shl(mulh16(*r.add(im), i32::from(*v.add(4 + (k & 3)))), 1);
        }
    }
}

/// `acc += step`, then with `c0`-`c3` the rounded upper halves of `acc`'s four
/// `[re x4, im x4]` Q30 pairs per group, `l, r = 4 * (l * c0 + r * c1),
/// 4 * (l * c2 + r * c3)`.
unsafe fn ps_mix_c(l: *mut i32, r: *mut i32, acc: *mut i32, step: *const i32, groups: i32) {
    unsafe {
        for g in 0..groups as usize {
            let mut c = [[0i16; 8]; 4];
            for j in 0..4 {
                for n in 0..8 {
                    let at = 32 * g + 8 * j + n;
                    *acc.add(at) = add(*acc.add(at), *step.add(at));
                    c[j][n] = (add(*acc.add(at), 0x8000) >> 16) as i16;
                }
            }
            for n in 0..4 {
                let k = 4 * g + n;
                let (re, im) = (lane(k, 0), lane(k, 1));
                let (lr, li, rr, ri) = (*l.add(re), *l.add(im), *r.add(re), *r.add(im));
                let (ar, ai) = cmul_vec(lr, li, c[0][n], c[0][4 + n]);
                let (br, bi) = cmul_vec(rr, ri, c[1][n], c[1][4 + n]);
                let (cr, ci) = cmul_vec(lr, li, c[2][n], c[2][4 + n]);
                let (dr, di) = cmul_vec(rr, ri, c[3][n], c[3][4 + n]);
                *l.add(re) = shl(add(ar, br), 2);
                *l.add(im) = shl(add(ai, bi), 2);
                *r.add(re) = shl(add(cr, dr), 2);
                *r.add(im) = shl(add(ci, di), 2);
            }
        }
    }
}

/// [`ps_mix`] with only the re halves of the pairs, which it alone steps.
unsafe fn ps_mix_real_c(l: *mut i32, r: *mut i32, acc: *mut i32, step: *const i32, groups: i32) {
    unsafe {
        for g in 0..groups as usize {
            let mut c = [[0i32; 4]; 4];
            for j in 0..4 {
                for n in 0..4 {
                    let at = 32 * g + 8 * j + n;
                    *acc.add(at) = add(*acc.add(at), *step.add(at));
                    c[j][n] = i32::from((add(*acc.add(at), 0x8000) >> 16) as i16);
                }
            }
            for n in 0..4 {
                for p in 0..2 {
                    let i = 8 * g + 4 * p + n;
                    let (x, y) = (*l.add(i), *r.add(i));
                    *l.add(i) = shl(add(mulh16(x, c[0][n]), mulh16(y, c[1][n])), 2);
                    *r.add(i) = shl(add(mulh16(x, c[2][n]), mulh16(y, c[3][n])), 2);
                }
            }
        }
    }
}

/// From interleaved re/im pairs to the group layout.
unsafe fn ps_unzip_c(dst: *mut i32, src: *const i32, groups: i32) {
    unsafe {
        for g in 0..groups as usize {
            for n in 0..4 {
                *dst.add(8 * g + n) = *src.add(8 * g + 2 * n);
                *dst.add(8 * g + 4 + n) = *src.add(8 * g + 2 * n + 1);
            }
        }
    }
}

/// From the group layout to interleaved re/im pairs.
unsafe fn ps_zip_c(dst: *mut i32, src: *const i32, groups: i32) {
    unsafe {
        for g in 0..groups as usize {
            for n in 0..4 {
                *dst.add(8 * g + 2 * n) = *src.add(8 * g + n);
                *dst.add(8 * g + 2 * n + 1) = *src.add(8 * g + 4 + n);
            }
        }
    }
}

/// `a[k * 4 + l] = 16 * re(x[l][k])`, `b[(63 - k) * 4 + l] = 16 * im(x[l][k])`.
unsafe fn syn_in4_c(a: *mut i32, b: *mut i32, x: *const *const i32) {
    unsafe {
        for l in 0..4 {
            let row = *x.add(l);
            for k in 0..64 {
                *a.add(k * 4 + l) = (*row.add(2 * k)).wrapping_mul(16);
                *b.add((63 - k) * 4 + l) = (*row.add(2 * k + 1)).wrapping_mul(16);
            }
        }
    }
}

/// The synthesis `v` of four slots from their DCT-IV halves, split into hi/lo;
/// slot `l` lands at `hi - 128 * (l + 1)` and `lo - 128 * (l + 1)`.
unsafe fn syn_v4_c(hi: *mut i16, lo: *mut i16, a: *const i32, b: *const i32) {
    unsafe {
        for l in 0..4 {
            let h = hi.sub(128 * (l + 1));
            let o = lo.sub(128 * (l + 1));
            for n in 0..64 {
                let an = *a.add(n * 4 + l);
                let bn = if n & 1 != 0 { (*b.add(n * 4 + l)).wrapping_neg() } else { *b.add(n * 4 + l) };
                split(h.add(n), o.add(n), sub(bn, an).wrapping_mul(2));
                split(h.add(127 - n), o.add(127 - n), add(an, bn).wrapping_mul(2));
            }
        }
    }
}

#[inline(always)]
fn pack_one(v: i32, shift: u32) -> i16 {
    (add(v, 1 << (shift - 1)) >> shift).clamp(-32768, 32767) as i16
}

/// `(v + (1 << (shift - 1))) >> shift` saturated to 16 bits, `n` samples a
/// multiple of 8, interleaved with `b` when `b` is not null.
unsafe fn pack_c(out: *mut i16, a: *const i32, b: *const i32, n: i32, shift: i32) {
    let (n, shift) = (n as usize, shift as u32);
    unsafe {
        if b.is_null() {
            for i in 0..n {
                *out.add(i) = pack_one(*a.add(i), shift);
            }
            return;
        }
        for i in 0..n {
            *out.add(2 * i) = pack_one(*a.add(i), shift);
            *out.add(2 * i + 1) = pack_one(*b.add(i), shift);
        }
    }
}

macro_rules! kernels {
    ($($name:ident / $sym:ident / $scalar:ident ($($a:ident: $t:ty),*);)*) => {
        mod p4 {
            unsafe extern "C" {
                pub safe fn devkit_aac_k_prepare();
                $(pub fn $sym($($a: $t),*);)*
            }
        }

        $(
            #[inline(always)]
            pub(super) unsafe fn $name($($a: $t),*) {
                if HAVE_PIE {
                    unsafe { p4::$sym($($a),*) };
                } else {
                    unsafe { $scalar($($a),*) };
                }
            }
        )*
    };
}

kernels! {
    qmf_analysis / devkit_aac_k_qmf_analysis / qmf_analysis_c (u: *mut i32, hi: *const i16, lo: *const i16, c: *const i16);
    qmf_synthesis / devkit_aac_k_qmf_synthesis / qmf_synthesis_c (acc: *mut i32, hi: *const i16, lo: *const i16, c: *const i16);
    pack / devkit_aac_k_pack / pack_c (out: *mut i16, a: *const i32, b: *const i32, n: i32, shift: i32);
    dct4_64x4 / devkit_aac_k_dct4_64x4 / dct4_64x4_c (x: *mut i32, tmp: *mut i32, t: *const *const i16);
    dct4_1024 / devkit_aac_k_dct4_1024 / dct4_1024_c (x: *mut i32, tmp: *mut i32, t: *const *const i16);
    ps_allpass / devkit_aac_k_ps_allpass / ps_allpass_c (r: *mut i32, s: *const i32, ap: *mut i32, w: i32);
    ps_gain / devkit_aac_k_ps_gain / ps_gain_c (r: *mut i32, g: *const i16, groups: i32);
    ps_mix / devkit_aac_k_ps_mix / ps_mix_c (l: *mut i32, r: *mut i32, acc: *mut i32, step: *const i32, groups: i32);
    ps_mix_real / devkit_aac_k_ps_mix_real / ps_mix_real_c (l: *mut i32, r: *mut i32, acc: *mut i32, step: *const i32, groups: i32);
    ps_unzip / devkit_aac_k_ps_unzip / ps_unzip_c (dst: *mut i32, src: *const i32, groups: i32);
    ps_zip / devkit_aac_k_ps_zip / ps_zip_c (dst: *mut i32, src: *const i32, groups: i32);
    syn_in4 / devkit_aac_k_syn_in4 / syn_in4_c (a: *mut i32, b: *mut i32, x: *const *const i32);
    syn_v4 / devkit_aac_k_syn_v4 / syn_v4_c (hi: *mut i16, lo: *mut i16, a: *const i32, b: *const i32);
}

#[inline(always)]
pub(super) fn prepare() {
    if HAVE_PIE {
        p4::devkit_aac_k_prepare();
    }
}

#[repr(C)]
pub(super) struct RamTables {
    qmf_c: A16<[i16; 640]>,
    qmf_c2r: A16<[i16; 320]>,
    dct64_pre: A16<[i16; 256]>,
    dct64_fft: A16<[i16; 128]>,
    dct64_post: A16<[i16; 256]>,
    dct1024_pre: A16<[i16; 1024]>,
    dct1024_fft: A16<[i16; 512]>,
    dct1024_mid: A16<[i16; 768]>,
    dct1024_post: A16<[i16; 1024]>,
    win_long: [[i16; 1024]; 2],
}

unsafe impl Zeroable for RamTables {}

/// Where the transforms read their tables from. HE-AAC walks these every frame
/// and a line missed in flash costs far more than one in the decoder's memory.
#[derive(Clone, Copy)]
pub(super) struct Tables {
    pub qmf_c: *const i16,
    pub qmf_c2r: *const i16,
    pub dct64: [*const i16; 3],
    pub dct1024: [*const i16; 4],
    pub win_long: [*const [i16; 1024]; 2],
}

impl Tables {
    pub(super) fn rom() -> Self {
        Self {
            qmf_c: SBR_QMF_C.as_ptr(),
            qmf_c2r: SBR_QMF_C2R.as_ptr(),
            dct64: [
                devkit_aac_pie_dct64_pre.as_ptr(),
                devkit_aac_pie_fft32.as_ptr(),
                devkit_aac_pie_dct64_post.as_ptr(),
            ],
            dct1024: [
                devkit_aac_pie_dct1024_pre.as_ptr(),
                devkit_aac_pie_dct1024_fft128.as_ptr(),
                devkit_aac_pie_dct1024_mid.as_ptr(),
                devkit_aac_pie_dct1024_post.as_ptr(),
            ],
            win_long: [&AAC_WIN_SINE_LONG, &AAC_WIN_KBD_LONG],
        }
    }

    pub(super) fn ram(t: &mut RamTables) -> Self {
        t.qmf_c = SBR_QMF_C;
        t.qmf_c2r = SBR_QMF_C2R;
        t.dct64_pre = devkit_aac_pie_dct64_pre;
        t.dct64_fft = devkit_aac_pie_fft32;
        t.dct64_post = devkit_aac_pie_dct64_post;
        t.dct1024_pre = devkit_aac_pie_dct1024_pre;
        t.dct1024_fft = devkit_aac_pie_dct1024_fft128;
        t.dct1024_mid = devkit_aac_pie_dct1024_mid;
        t.dct1024_post = devkit_aac_pie_dct1024_post;
        t.win_long = [AAC_WIN_SINE_LONG, AAC_WIN_KBD_LONG];
        Self {
            qmf_c: t.qmf_c.as_ptr(),
            qmf_c2r: t.qmf_c2r.as_ptr(),
            dct64: [t.dct64_pre.as_ptr(), t.dct64_fft.as_ptr(), t.dct64_post.as_ptr()],
            dct1024: [
                t.dct1024_pre.as_ptr(),
                t.dct1024_fft.as_ptr(),
                t.dct1024_mid.as_ptr(),
                t.dct1024_post.as_ptr(),
            ],
            win_long: [&t.win_long[0], &t.win_long[1]],
        }
    }
}

fn next(s: &mut u32) -> u32 {
    *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
    *s
}

#[repr(C)]
struct Selftest {
    hi: A16<[i16; 2560]>,
    lo: A16<[i16; 2560]>,
    a: A16<[i32; 64]>,
    ra: A16<[i32; 64]>,
    pa: A16<[i16; 128]>,
    pb: A16<[i16; 128]>,
    d1: A16<[i32; 256]>,
    d2: A16<[i32; 256]>,
    w: A16<[i32; 256]>,
    rows: A16<[i32; 512]>,
    ia: A16<[i32; 256]>,
    ib: A16<[i32; 256]>,
    ja: A16<[i32; 256]>,
    jb: A16<[i32; 256]>,
    vh: A16<[[i16; 640]; 2]>,
    vl: A16<[[i16; 640]; 2]>,
    l1: A16<[i32; 1024]>,
    l2: A16<[i32; 1024]>,
    lt: A16<[i32; 1024]>,
    xa: A16<[[i32; 144]; 2]>,
    xb: A16<[[i32; 144]; 2]>,
    ps: A16<[i32; 64]>,
    pap: A16<[[i32; 3 * 8 * 64]; 2]>,
    pg: A16<[i16; G * 8]>,
    pacc: A16<[[i32; G * 32]; 2]>,
    pstep: A16<[i32; G * 32]>,
}

unsafe impl Zeroable for Selftest {}

/// Runs the SIMD kernels against their scalar versions on random input and
/// returns the number of mismatching calls, always 0 where there is no SIMD
/// version.
pub fn kernel_selftest(seed: u32, iterations: u32) -> Result<u32> {
    if !HAVE_PIE {
        return Ok(0);
    }
    let tables = Tables::rom();
    let mut seed = seed;
    let mut t: Box<Selftest, Caps> = heap::zeroed_box(Caps::DEFAULT)?;
    let t = &mut *t;
    let mut bad = 0u32;
    let g = G as i32;
    prepare();
    for _ in 0..iterations {
        let bits = 8 + next(&mut seed) % 18;
        unsafe {
            for i in 0..2560 {
                let v = (next(&mut seed) as i32) >> (31 - bits);
                split(t.hi.as_mut_ptr().add(i), t.lo.as_mut_ptr().add(i), v);
            }
            let (hi, lo) = (t.hi.as_ptr(), t.lo.as_ptr());
            qmf_synthesis(t.a.as_mut_ptr(), hi, lo, SBR_QMF_C.as_ptr());
            qmf_synthesis_c(t.ra.as_mut_ptr(), hi, lo, SBR_QMF_C.as_ptr());
            bad += u32::from(t.a.0 != t.ra.0);
            qmf_analysis(t.a.as_mut_ptr(), hi.add(512), lo.add(512), SBR_QMF_C2R.as_ptr());
            qmf_analysis_c(t.ra.as_mut_ptr(), hi.add(512), lo.add(512), SBR_QMF_C2R.as_ptr());
            bad += u32::from(t.a.0 != t.ra.0);
            for i in 0..64 {
                t.a[i] = (next(&mut seed) as i32) >> (31 - bits - 4);
                t.ra[i] = (next(&mut seed) as i32) >> (31 - bits - 4);
            }
            let shift = 1 + (next(&mut seed) % 10) as i32;
            pack(t.pa.as_mut_ptr(), t.a.as_ptr(), t.ra.as_ptr(), 64, shift);
            pack_c(t.pb.as_mut_ptr(), t.a.as_ptr(), t.ra.as_ptr(), 64, shift);
            bad += u32::from(t.pa.0 != t.pb.0);
            pack(t.pa.as_mut_ptr(), t.a.as_ptr(), core::ptr::null(), 64, shift);
            pack_c(t.pb.as_mut_ptr(), t.a.as_ptr(), core::ptr::null(), 64, shift);
            bad += u32::from(t.pa[..64] != t.pb[..64]);
            for i in 0..256 {
                let v = (next(&mut seed) as i32) >> (31 - bits - 3);
                t.d1[i] = v;
                t.d2[i] = v;
            }
            dct4_64x4(t.d1.as_mut_ptr(), t.w.as_mut_ptr(), tables.dct64.as_ptr());
            dct4_64x4_c(t.d2.as_mut_ptr(), t.w.as_mut_ptr(), tables.dct64.as_ptr());
            bad += u32::from(t.d1.0 != t.d2.0);
            for i in 0..512 {
                t.rows[i] = (next(&mut seed) as i32) >> (31 - bits);
            }
            let r = t.rows.as_ptr();
            let rp = [r, r.add(128), r.add(256), r.add(384)];
            syn_in4(t.ia.as_mut_ptr(), t.ib.as_mut_ptr(), rp.as_ptr());
            syn_in4_c(t.ja.as_mut_ptr(), t.jb.as_mut_ptr(), rp.as_ptr());
            bad += u32::from(t.ia.0 != t.ja.0 || t.ib.0 != t.jb.0);
            t.vh.0 = [[0; 640]; 2];
            t.vl.0 = [[0; 640]; 2];
            syn_v4(t.vh[0].as_mut_ptr().add(576), t.vl[0].as_mut_ptr().add(576), t.d1.as_ptr(), t.ia.as_ptr());
            syn_v4_c(t.vh[1].as_mut_ptr().add(576), t.vl[1].as_mut_ptr().add(576), t.d1.as_ptr(), t.ia.as_ptr());
            bad += u32::from(t.vh[0] != t.vh[1] || t.vl[0] != t.vl[1]);
            for i in 0..1024 {
                let v = (next(&mut seed) as i32) >> (31 - bits - 4);
                t.l1[i] = v;
                t.l2[i] = v;
            }
            dct4_1024(t.l1.as_mut_ptr(), t.lt.as_mut_ptr(), tables.dct1024.as_ptr());
            dct4_1024_c(t.l2.as_mut_ptr(), t.lt.as_mut_ptr(), tables.dct1024.as_ptr());
            bad += u32::from(t.l1.0 != t.l2.0);
            for i in 0..144 {
                let v = (next(&mut seed) as i32) >> (31 - bits - 2);
                t.xa[0][i] = v;
                t.xa[1][i] = v;
                let v = (next(&mut seed) as i32) >> (31 - bits - 2);
                t.xb[0][i] = v;
                t.xb[1][i] = v;
            }
            for i in 0..64 {
                t.ps[i] = (next(&mut seed) as i32) >> (31 - bits - 2);
            }
            for i in 0..3 * 8 * 64 {
                let v = (next(&mut seed) as i32) >> (31 - bits - 2);
                t.pap[0][i] = v;
                t.pap[1][i] = v;
            }
            for i in 0..G * 8 {
                t.pg[i] = next(&mut seed) as i16;
            }
            for i in 0..G * 32 {
                let v = (next(&mut seed) as i32) >> 1;
                t.pacc[0][i] = v;
                t.pacc[1][i] = v;
                t.pstep[i] = (next(&mut seed) as i32) >> 6;
            }
            let w = (next(&mut seed) & 7) as i32;
            ps_allpass(t.xa[0].as_mut_ptr(), t.ps.as_ptr(), t.pap[0].as_mut_ptr(), w);
            ps_allpass_c(t.xa[1].as_mut_ptr(), t.ps.as_ptr(), t.pap[1].as_mut_ptr(), w);
            bad += u32::from(t.xa[0] != t.xa[1]);
            bad += u32::from(t.pap[0] != t.pap[1]);
            ps_gain(t.xa[0].as_mut_ptr(), t.pg.as_ptr(), g);
            ps_gain_c(t.xa[1].as_mut_ptr(), t.pg.as_ptr(), g);
            bad += u32::from(t.xa[0] != t.xa[1]);
            ps_mix(t.xa[0].as_mut_ptr(), t.xb[0].as_mut_ptr(), t.pacc[0].as_mut_ptr(), t.pstep.as_ptr(), g);
            ps_mix_c(t.xa[1].as_mut_ptr(), t.xb[1].as_mut_ptr(), t.pacc[1].as_mut_ptr(), t.pstep.as_ptr(), g);
            bad += u32::from(t.pacc[0] != t.pacc[1]);
            ps_mix_real(t.xa[0].as_mut_ptr(), t.xb[0].as_mut_ptr(), t.pacc[0].as_mut_ptr(), t.pstep.as_ptr(), g);
            ps_mix_real_c(t.xa[1].as_mut_ptr(), t.xb[1].as_mut_ptr(), t.pacc[1].as_mut_ptr(), t.pstep.as_ptr(), g);
            bad += u32::from(t.xa[0] != t.xa[1] || t.xb[0] != t.xb[1]);
            bad += u32::from(t.pacc[0] != t.pacc[1]);
            ps_unzip(t.xb[0].as_mut_ptr(), t.xa[0].as_ptr(), g);
            ps_unzip_c(t.xb[1].as_mut_ptr(), t.xa[0].as_ptr(), g);
            bad += u32::from(t.xb[0] != t.xb[1]);
            ps_zip(t.xb[0].as_mut_ptr(), t.xa[0].as_ptr(), g);
            ps_zip_c(t.xb[1].as_mut_ptr(), t.xa[0].as_ptr(), g);
            bad += u32::from(t.xb[0] != t.xb[1]);
            bad += u32::from(t.xa[0] != t.xa[1] || t.xb[0] != t.xb[1]);
        }
    }
    Ok(bad)
}
