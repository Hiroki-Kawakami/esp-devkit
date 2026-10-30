/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::heap::{self, Caps};

use super::rc::ilog;
use super::tables::CELT_WINDOW;
use super::{CELT_OVERLAP, mulh, sat_sig};
use crate::Result;

const SIZES: usize = 4;
const FFT_SIZES: [usize; SIZES] = [480, 240, 120, 60];

// cos(pi * x) with plain double arithmetic, so the tables come out the same on
// every target.
fn cos_pi(x: f64) -> f64 {
    let mut x = x - 2.0 * ((x / 2.0) as i64 as f64);
    if x < 0.0 {
        x = -x;
    }
    if x > 1.0 {
        x = 2.0 - x;
    }
    let mut sign = 1.0;
    if x > 0.5 {
        x = 1.0 - x;
        sign = -1.0;
    }
    let t = x * 3.14159265358979323846;
    let t2 = t * t;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..=12 {
        term *= -t2 / f64::from((2 * k - 1) * (2 * k));
        sum += term;
    }
    sign * sum
}

fn sin_pi(x: f64) -> f64 {
    cos_pi(0.5 - x)
}

fn q31(v: f64) -> i32 {
    let r = v * 2147483648.0;
    let r = if r < 0.0 { r - 0.5 } else { r + 0.5 };
    if r > 2147483647.0 {
        i32::MAX
    } else if r < -2147483648.0 {
        i32::MIN
    } else {
        r as i32
    }
}

struct Fft {
    n: usize,
    stages: usize,
    radix: [u8; 8],
    tw: Box<[i32], Caps>,
    perm: Box<[u16], Caps>,
}

impl Fft {
    fn new(n: usize, caps: Caps) -> Result<Self> {
        let mut radix = [0u8; 8];
        let mut stages = 0;
        let mut rest = n;
        while rest > 1 {
            let p = match rest {
                r if r % 4 == 0 => 4,
                r if r % 2 == 0 => 2,
                r if r % 3 == 0 => 3,
                _ => 5,
            };
            radix[stages] = p as u8;
            stages += 1;
            rest /= p;
        }
        let mut tw: Box<[i32], Caps> = heap::zeroed_slice(n * 2, caps)?;
        let mut perm: Box<[u16], Caps> = heap::zeroed_slice(n, caps)?;
        for k in 0..n {
            let a = 2.0 * k as f64 / n as f64;
            tw[2 * k] = q31(cos_pi(a));
            tw[2 * k + 1] = q31(-sin_pi(a));
        }
        for pos in 0..n {
            let (mut rest, mut len, mut stride, mut index) = (pos, n, 1, 0);
            for &p in &radix[..stages] {
                let p = usize::from(p);
                len /= p;
                index += rest / len * stride;
                rest %= len;
                stride *= p;
            }
            perm[pos] = index as u16;
        }
        Ok(Self { n, stages, radix, tw, perm })
    }
}

#[inline(always)]
fn mulq31(a: i32, c: i32) -> i32 {
    mulh(a, c).wrapping_mul(2)
}

#[inline(always)]
fn cmul(ar: i32, ai: i32, wr: i32, wi: i32) -> (i32, i32) {
    (
        mulh(ar, wr).wrapping_sub(mulh(ai, wi)).wrapping_mul(2),
        mulh(ar, wi).wrapping_add(mulh(ai, wr)).wrapping_mul(2),
    )
}

#[inline(always)]
unsafe fn twiddle(v: *const i32, w: *const i32, k: usize) -> (i32, i32) {
    unsafe { if k == 0 { (*v, *v.add(1)) } else { cmul(*v, *v.add(1), *w, *w.add(1)) } }
}

macro_rules! add {
    ($a:expr, $b:expr) => {
        ($a).wrapping_add($b)
    };
}

macro_rules! sub {
    ($a:expr, $b:expr) => {
        ($a).wrapping_sub($b)
    };
}

unsafe fn stage2(x: *mut i32, n: usize, m: usize, tw: *const i32, step: usize) {
    unsafe {
        for k in 0..m {
            let w1 = tw.add(2 * k * step);
            let mut g = k;
            while g < n {
                let o0 = x.add(2 * g);
                let o1 = o0.add(2 * m);
                let (r1, i1) = twiddle(o1, w1, k);
                let (r0, i0) = (*o0, *o0.add(1));
                *o0 = add!(r0, r1);
                *o0.add(1) = add!(i0, i1);
                *o1 = sub!(r0, r1);
                *o1.add(1) = sub!(i0, i1);
                g += 2 * m;
            }
        }
    }
}

unsafe fn stage3(x: *mut i32, n: usize, m: usize, tw: *const i32, step: usize) {
    // sin(2pi/3) in Q31; cos(2pi/3) is -1/2
    const S3: i32 = 1859775393;
    unsafe {
        for k in 0..m {
            let (w1, w2) = (tw.add(2 * k * step), tw.add(4 * k * step));
            let mut g = k;
            while g < n {
                let o0 = x.add(2 * g);
                let o1 = o0.add(2 * m);
                let o2 = o1.add(2 * m);
                let (r1, i1) = twiddle(o1, w1, k);
                let (r2, i2) = twiddle(o2, w2, k);
                let (r0, i0) = (*o0, *o0.add(1));
                let (sr, si) = (add!(r1, r2), add!(i1, i2));
                let (dr, di) = (sub!(r1, r2), sub!(i1, i2));
                let (mr, mi) = (sub!(r0, sr >> 1), sub!(i0, si >> 1));
                let (tr, ti) = (mulq31(di, S3), mulq31(dr, S3));
                *o0 = add!(r0, sr);
                *o0.add(1) = add!(i0, si);
                *o1 = add!(mr, tr);
                *o1.add(1) = sub!(mi, ti);
                *o2 = sub!(mr, tr);
                *o2.add(1) = add!(mi, ti);
                g += 3 * m;
            }
        }
    }
}

unsafe fn stage4(x: *mut i32, n: usize, m: usize, tw: *const i32, step: usize) {
    unsafe {
        for k in 0..m {
            let (w1, w2, w3) = (tw.add(2 * k * step), tw.add(4 * k * step), tw.add(6 * k * step));
            let mut g = k;
            while g < n {
                let o0 = x.add(2 * g);
                let o1 = o0.add(2 * m);
                let o2 = o1.add(2 * m);
                let o3 = o2.add(2 * m);
                let (r1, i1) = twiddle(o1, w1, k);
                let (r2, i2) = twiddle(o2, w2, k);
                let (r3, i3) = twiddle(o3, w3, k);
                let (r0, i0) = (*o0, *o0.add(1));
                let (ar, ai) = (add!(r0, r2), add!(i0, i2));
                let (br, bi) = (sub!(r0, r2), sub!(i0, i2));
                let (cr, ci) = (add!(r1, r3), add!(i1, i3));
                let (dr, di) = (sub!(r1, r3), sub!(i1, i3));
                *o0 = add!(ar, cr);
                *o0.add(1) = add!(ai, ci);
                *o2 = sub!(ar, cr);
                *o2.add(1) = sub!(ai, ci);
                *o1 = add!(br, di);
                *o1.add(1) = sub!(bi, dr);
                *o3 = sub!(br, di);
                *o3.add(1) = add!(bi, dr);
                g += 4 * m;
            }
        }
    }
}

unsafe fn stage5(x: *mut i32, n: usize, m: usize, tw: *const i32, step: usize) {
    // cos and sin of 2pi/5 and 4pi/5, Q31
    const C1: i32 = 663608942;
    const S1: i32 = 2042378317;
    const C2: i32 = -1737350766;
    const S2: i32 = 1262259218;
    unsafe {
        for k in 0..m {
            let (w1, w2) = (tw.add(2 * k * step), tw.add(4 * k * step));
            let (w3, w4) = (tw.add(6 * k * step), tw.add(8 * k * step));
            let mut g = k;
            while g < n {
                let o0 = x.add(2 * g);
                let o1 = o0.add(2 * m);
                let o2 = o1.add(2 * m);
                let o3 = o2.add(2 * m);
                let o4 = o3.add(2 * m);
                let (r1, i1) = twiddle(o1, w1, k);
                let (r2, i2) = twiddle(o2, w2, k);
                let (r3, i3) = twiddle(o3, w3, k);
                let (r4, i4) = twiddle(o4, w4, k);
                let (r0, i0) = (*o0, *o0.add(1));
                let (ar, ai) = (add!(r1, r4), add!(i1, i4));
                let (br, bi) = (sub!(r1, r4), sub!(i1, i4));
                let (cr, ci) = (add!(r2, r3), add!(i2, i3));
                let (dr, di) = (sub!(r2, r3), sub!(i2, i3));
                let m1r = add!(add!(r0, mulq31(ar, C1)), mulq31(cr, C2));
                let m1i = add!(add!(i0, mulq31(ai, C1)), mulq31(ci, C2));
                let m2r = add!(add!(r0, mulq31(ar, C2)), mulq31(cr, C1));
                let m2i = add!(add!(i0, mulq31(ai, C2)), mulq31(ci, C1));
                let n1r = add!(mulq31(bi, S1), mulq31(di, S2));
                let n1i = add!(mulq31(br, S1), mulq31(dr, S2));
                let n2r = sub!(mulq31(bi, S2), mulq31(di, S1));
                let n2i = sub!(mulq31(br, S2), mulq31(dr, S1));
                *o0 = add!(add!(r0, ar), cr);
                *o0.add(1) = add!(add!(i0, ai), ci);
                *o1 = add!(m1r, n1r);
                *o1.add(1) = sub!(m1i, n1i);
                *o4 = sub!(m1r, n1r);
                *o4.add(1) = add!(m1i, n1i);
                *o2 = add!(m2r, n2r);
                *o2.add(1) = sub!(m2i, n2i);
                *o3 = sub!(m2r, n2r);
                *o3.add(1) = add!(m2i, n2i);
                g += 5 * m;
            }
        }
    }
}

unsafe fn fft_stages(f: &Fft, x: *mut i32) {
    let n = f.n;
    let tw = f.tw.as_ptr();
    let mut m = 1;
    for s in (0..f.stages).rev() {
        let p = usize::from(f.radix[s]);
        let step = n / (p * m);
        unsafe {
            match p {
                2 => stage2(x, n, m, tw, step),
                3 => stage3(x, n, m, tw, step),
                4 => stage4(x, n, m, tw, step),
                _ => stage5(x, n, m, tw, step),
            }
        }
        m *= p;
    }
}

/// Index `s` is the transform of `1920 >> s` samples.
pub(super) struct Mdct {
    fft: [Fft; SIZES],
    rot: [Box<[i32], Caps>; SIZES],
    buf: Box<[i32], Caps>,
}

fn rot_table(n4: usize, caps: Caps) -> Result<Box<[i32], Caps>> {
    let mut rot: Box<[i32], Caps> = heap::zeroed_slice(n4 * 2, caps)?;
    for k in 0..n4 {
        let a = (k as f64 + 0.125) / (2 * n4) as f64;
        rot[2 * k] = q31(cos_pi(a));
        rot[2 * k + 1] = q31(-sin_pi(a));
    }
    Ok(rot)
}

impl Mdct {
    pub fn new(caps: Caps) -> Result<Self> {
        Ok(Self {
            fft: [Fft::new(FFT_SIZES[0], caps)?, Fft::new(FFT_SIZES[1], caps)?, Fft::new(FFT_SIZES[2], caps)?, Fft::new(FFT_SIZES[3], caps)?],
            rot: [rot_table(FFT_SIZES[0], caps)?, rot_table(FFT_SIZES[1], caps)?, rot_table(FFT_SIZES[2], caps)?, rot_table(FFT_SIZES[3], caps)?],
            buf: heap::zeroed_slice(2 * 960, caps)?,
        })
    }

    /// Inverse MDCT of `960 >> shift` coefficients read with `stride`, windowed
    /// into `out[0 .. n + CELT_OVERLAP)`: the first `CELT_OVERLAP` samples are
    /// added to what `out` holds, the rest overwritten.
    pub unsafe fn imdct(&mut self, input: *const i32, stride: usize, shift: usize, out: *mut i32) {
        let f = &self.fft[shift];
        let rot = self.rot[shift].as_ptr();
        let perm = f.perm.as_ptr();
        let window = CELT_WINDOW.as_ptr();
        let n4 = f.n;
        let n2 = 2 * n4;
        let z = self.buf.as_mut_ptr();
        unsafe {
            let u = z.add(n2);
            let mut l1 = 0u32;
            for k in 0..n2 {
                let a = (*input.add(k * stride)).unsigned_abs();
                l1 = l1.saturating_add(a);
            }
            let down = if l1 >= 1 << 30 { ilog(l1) - 30 } else { 0 };

            for pos in 0..n4 {
                let k = usize::from(*perm.add(pos));
                let re = *input.add(2 * k * stride) >> down;
                let im = *input.add((n2 - 1 - 2 * k) * stride) >> down;
                let (a, b) = cmul(re, im, *rot.add(2 * k), *rot.add(2 * k + 1));
                *z.add(2 * pos) = a;
                *z.add(2 * pos + 1) = b;
            }
            fft_stages(f, z);
            for k in 0..n4 {
                let (re, im) = cmul(*z.add(2 * k), *z.add(2 * k + 1), *rot.add(2 * k), *rot.add(2 * k + 1));
                *u.add(2 * k) = re;
                *u.add(n2 - 1 - 2 * k) = im.wrapping_neg();
            }

            // y[n] of the 2 * n2 output, windowed and written from n = (n2 - overlap) / 2
            let lead = (n2 - CELT_OVERLAP) / 2;
            let half = n2 / 2;
            for j in 0..n2 + CELT_OVERLAP {
                let n = j + lead;
                let mut y = if n < half {
                    *u.add(n + half)
                } else if n < 3 * half {
                    (*u.add(3 * half - 1 - n)).wrapping_neg()
                } else {
                    (*u.add(n - 3 * half)).wrapping_neg()
                };
                if down != 0 {
                    y = sat_sig(i64::from(y) << down);
                }
                if j < CELT_OVERLAP {
                    *out.add(j) = (*out.add(j)).wrapping_add(mulq31(y, i32::from(*window.add(j)) << 16));
                } else if j < n2 {
                    *out.add(j) = y;
                } else {
                    *out.add(j) = mulq31(y, i32::from(*window.add(n2 + CELT_OVERLAP - 1 - j)) << 16);
                }
            }
        }
    }
}
