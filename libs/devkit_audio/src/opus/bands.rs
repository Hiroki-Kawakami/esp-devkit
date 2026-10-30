/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit::math::{floorf, ldexpf, lrintf, sqrtf};
use idf::heap::{self, Caps};

use super::rc::{Rc, ilog};
use super::tables::*;
use super::{CELT_BANDS, MAX_PVQ_N, NORM_ONE, mulh};
use crate::Result;

pub(super) const BITRES: i32 = 3;
const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;
const SPREAD_NONE: i32 = 0;
const SPREAD_AGGRESSIVE: i32 = 3;

#[inline(always)]
pub(super) fn lcg(seed: u32) -> u32 {
    1664525u32.wrapping_mul(seed).wrapping_add(1013904223)
}

/// Plain float arithmetic only, so host and device round alike.
pub(super) fn exp2(x: f32) -> f32 {
    let i = floorf(x);
    let f = (x - i) * 0.69314718056;
    let mut term = 1.0f32;
    let mut sum = 1.0f32;
    for k in 1..=8 {
        term = term * f / k as f32;
        sum += term;
    }
    ldexpf(sum, i as i32)
}

// cos(pi / 2 * x) for 0 <= x <= 1
fn cos_half_pi(x: f32) -> f32 {
    let t = x * 1.57079632679;
    let t2 = t * t;
    let mut term = 1.0f32;
    let mut sum = 1.0f32;
    for k in 1..=7 {
        term = -term * t2 / ((2 * k - 1) * (2 * k)) as f32;
        sum += term;
    }
    sum
}

#[inline(always)]
fn mul_q15(a: i32, c: i32) -> i32 {
    ((i64::from(a) * i64::from(c) + 16384) >> 15) as i32
}

#[inline(always)]
fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + i32::from(a as i16) * i32::from(b as i16)) >> 15
}

fn bitexact_cos(x: i32) -> i32 {
    let x2 = (4096 + x * x) >> 13;
    1 + (32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)))
}

fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ilog(icos as u32);
    let ls = ilog(isin as u32);
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

fn isqrt32(mut v: u32) -> u32 {
    let mut g = 0u32;
    let mut shift = (ilog(v) - 1) >> 1;
    let mut b = 1u32 << shift;
    loop {
        let t = ((g << 1) + b) << shift;
        if t <= v {
            g += b;
            v -= t;
        }
        b >>= 1;
        shift -= 1;
        if shift < 0 {
            return g;
        }
    }
}

fn get_pulses(q: i32) -> i32 {
    if q < 8 { q } else { (8 + (q & 7)) << ((q >> 3) - 1) }
}

#[inline(always)]
fn pulse_cache(band: i32, lm: i32) -> &'static [u8] {
    &CELT_CACHE_BITS[CELT_CACHE_INDEX[((lm + 1) * CELT_BANDS as i32 + band) as usize] as usize..]
}

fn bits2pulses(band: i32, lm: i32, bits: i32) -> i32 {
    let cache = pulse_cache(band, lm);
    let (mut lo, mut hi) = (0, i32::from(cache[0]));
    let bits = bits - 1;
    for _ in 0..6 {
        let mid = (lo + hi + 1) >> 1;
        if i32::from(cache[mid as usize]) >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let low = if lo == 0 { -1 } else { i32::from(cache[lo as usize]) };
    if bits - low <= i32::from(cache[hi as usize]) - bits { lo } else { hi }
}

fn pulses2bits(band: i32, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 { 0 } else { i32::from(pulse_cache(band, lm)[pulses as usize]) + 1 }
}

/// V(n, k) of RFC 6716 4.3.4.2, the number of codewords of k pulses in n
/// dimensions, for every k a 32-bit codebook can have.
pub(super) struct Pvq {
    v: Box<[u32], Caps>,
    row: [u16; MAX_PVQ_N + 1],
}

impl Pvq {
    pub fn new(caps: Caps) -> Result<Self> {
        let mut prev = [0u64; 129];
        let mut cur = [0u64; 129];
        let mut count = 0u32;
        let mut row = [0u16; MAX_PVQ_N + 1];
        let mut max_ks = [0u8; MAX_PVQ_N + 1];
        for n in 0..=MAX_PVQ_N {
            for k in 0..=128 {
                cur[k] = if n == 0 {
                    u64::from(k == 0)
                } else if k == 0 {
                    1
                } else {
                    prev[k] + cur[k - 1] + prev[k - 1]
                };
                if cur[k] > u64::from(u32::MAX) {
                    cur[k] = 1 << 40;
                }
            }
            let mut max_k = 0;
            while max_k < 128 && cur[max_k + 1] <= u64::from(u32::MAX) {
                max_k += 1;
            }
            row[n] = count as u16;
            max_ks[n] = max_k as u8;
            count += max_k as u32 + 1;
            prev = cur;
        }
        let mut v: Box<[u32], Caps> = heap::zeroed_slice(count as usize, caps)?;
        for n in 0..=MAX_PVQ_N {
            let r = usize::from(row[n]);
            for k in 0..=usize::from(max_ks[n]) {
                v[r + k] = if n == 0 {
                    u32::from(k == 0)
                } else if k == 0 {
                    1
                } else {
                    let up = usize::from(row[n - 1]);
                    v[up + k].wrapping_add(v[r + k - 1]).wrapping_add(v[up + k - 1])
                };
            }
        }
        Ok(Self { v, row })
    }

    fn decode(&self, rc: &mut Rc<'_>, y: &mut [i32], n: usize, k: usize) {
        assert!(n <= MAX_PVQ_N && y.len() >= n);
        let mut k = k;
        let mut i = rc.uint(self.v[usize::from(self.row[n]) + k]);
        let v = self.v.as_ptr();
        for j in 0..n {
            unsafe {
                let low = v.add(usize::from(*self.row.get_unchecked(n - j - 1)));
                let cur = v.add(usize::from(*self.row.get_unchecked(n - j)));
                let mut p = (u64::from(*low.add(k)) + u64::from(*cur.add(k))) >> 1;
                let mut sign = 1;
                if u64::from(i) >= p {
                    sign = -1;
                    i -= p as u32;
                }
                let k0 = k;
                p -= u64::from(*low.add(k));
                while p > u64::from(i) {
                    k -= 1;
                    p -= u64::from(*low.add(k));
                }
                *y.get_unchecked_mut(j) = sign * (k0 - k) as i32;
                i -= p as u32;
            }
        }
    }
}

// c and s in Q31.
unsafe fn rotate_pairs(x: *mut i32, len: i32, stride: i32, c: i32, s: i32) {
    let stride = stride as usize;
    unsafe {
        let step = |p: *mut i32| {
            let (x1, x2) = (*p, *p.add(stride));
            *p.add(stride) = mulh(c, x2).wrapping_add(mulh(s, x1)).wrapping_mul(2);
            *p = mulh(c, x1).wrapping_sub(mulh(s, x2)).wrapping_mul(2);
        };
        for i in 0..len - stride as i32 {
            step(x.add(i as usize));
        }
        let mut i = len - 2 * stride as i32 - 1;
        while i >= 0 {
            step(x.add(i as usize));
            i -= 1;
        }
    }
}

unsafe fn unspread(x: *mut i32, len: i32, stride: i32, k: i32, spread: i32) {
    const FACTOR: [i32; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let gain = len as f32 / (len + FACTOR[spread as usize - 1] * k) as f32;
    let theta = 0.5 * gain * gain;
    let c = lrintf(cos_half_pi(theta) * 2147483520.0);
    let s = lrintf(cos_half_pi(1.0 - theta) * 2147483520.0);
    let mut stride2 = 0;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let len = len / stride;
    for i in 0..stride {
        unsafe {
            let at = x.add((i * len) as usize);
            if stride2 != 0 {
                rotate_pairs(at, len, stride2, s, c);
            }
            rotate_pairs(at, len, 1, c, s);
        }
    }
}

pub(super) unsafe fn renormalise(x: *mut i32, n: usize, gain: f32) {
    let x = unsafe { core::slice::from_raw_parts_mut(x, n) };
    let e: i64 = x.iter().map(|&v| i64::from(v) * i64::from(v)).sum();
    if e <= 0 {
        return;
    }
    let f = gain * NORM_ONE as f32 / sqrtf(e as f32);
    for v in x {
        *v = lrintf(*v as f32 * f);
    }
}

unsafe fn haar1(x: *mut i32, n0: i32, stride: i32) {
    const HALF_SQRT2: i32 = 1518500250;
    let (n0, stride) = ((n0 >> 1) as usize, stride as usize);
    unsafe {
        for i in 0..stride {
            for j in 0..n0 {
                let (p0, p1) = (x.add(stride * 2 * j + i), x.add(stride * (2 * j + 1) + i));
                let a = mulh(*p0, HALF_SQRT2).wrapping_mul(2);
                let b = mulh(*p1, HALF_SQRT2).wrapping_mul(2);
                *p0 = a.wrapping_add(b);
                *p1 = a.wrapping_sub(b);
            }
        }
    }
}

unsafe fn deinterleave_hadamard(x: *mut i32, tmp: *mut i32, n0: i32, stride: i32, hadamard: bool) {
    let (n0, stride) = (n0 as usize, stride as usize);
    let order = &CELT_HADAMARD_ORDER[stride.wrapping_sub(2).min(CELT_HADAMARD_ORDER.len())..];
    unsafe {
        for i in 0..stride {
            let at = if hadamard { usize::from(order[i]) } else { i };
            for j in 0..n0 {
                *tmp.add(at * n0 + j) = *x.add(j * stride + i);
            }
        }
        core::ptr::copy_nonoverlapping(tmp, x, n0 * stride);
    }
}

unsafe fn interleave_hadamard(x: *mut i32, tmp: *mut i32, n0: i32, stride: i32, hadamard: bool) {
    let (n0, stride) = (n0 as usize, stride as usize);
    let order = &CELT_HADAMARD_ORDER[stride.wrapping_sub(2).min(CELT_HADAMARD_ORDER.len())..];
    unsafe {
        for i in 0..stride {
            let at = if hadamard { usize::from(order[i]) } else { i };
            for j in 0..n0 {
                *tmp.add(j * stride + i) = *x.add(at * n0 + j);
            }
        }
        core::ptr::copy_nonoverlapping(tmp, x, n0 * stride);
    }
}

unsafe fn stereo_merge(x: *mut i32, y: *mut i32, mid: f32, n: usize) {
    let (x, y) = unsafe { (core::slice::from_raw_parts_mut(x, n), core::slice::from_raw_parts_mut(y, n)) };
    let (mut xp, mut side) = (0i64, 0i64);
    for j in 0..n {
        xp += i64::from(x[j]) * i64::from(y[j]);
        side += i64::from(y[j]) * i64::from(y[j]);
    }
    let scale = 1.0 / (NORM_ONE as f32 * NORM_ONE as f32);
    let fxp = mid * xp as f32 * scale;
    let fside = side as f32 * scale;
    let el = mid * mid + fside - 2.0 * fxp;
    let er = mid * mid + fside + 2.0 * fxp;
    if er < 6e-4 || el < 6e-4 {
        y.copy_from_slice(x);
        return;
    }
    let lgain = 1.0 / sqrtf(el);
    let rgain = 1.0 / sqrtf(er);
    for j in 0..n {
        let l = mid * x[j] as f32;
        let r = y[j] as f32;
        x[j] = lrintf(lgain * (l - r));
        y[j] = lrintf(rgain * (l + r));
    }
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let qb = (b - pulse_cap - (4 << BITRES)).min((b + n2 * offset) / n2).min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        return 1;
    }
    let qn = i32::from(CELT_EXP2_FRAC8[(qb & 7) as usize]) >> (14 - (qb >> BITRES));
    (qn + 1) >> 1 << 1
}

pub(super) struct Bands<'a, 'd> {
    pub rc: &'a mut Rc<'d>,
    pub pvq: &'a Pvq,
    pub lm: i32,
    pub spread: i32,
    pub intensity: i32,
    pub stereo_inversion: bool,
    pub remaining_bits: i32,
    pub seed: u32,
    pub scratch: *mut i32,
    pub lowband_scratch: *mut i32,
    pub iy: *mut i32,
    pub coded_bands: i32,
}

impl Bands<'_, '_> {
    unsafe fn pvq_unquant(&mut self, x: *mut i32, n: i32, k: i32, b_count: i32, gain: f32) -> u32 {
        let iy = unsafe { core::slice::from_raw_parts_mut(self.iy, n as usize) };
        self.pvq.decode(self.rc, iy, n as usize, k as usize);
        let ryy: i64 = iy.iter().map(|&v| i64::from(v) * i64::from(v)).sum();
        let f = gain * NORM_ONE as f32 / sqrtf(ryy as f32);
        unsafe {
            for (i, &v) in iy.iter().enumerate() {
                *x.add(i) = lrintf(v as f32 * f);
            }
            unspread(x, n, b_count, k, self.spread);
        }
        if b_count <= 1 {
            return 1;
        }
        let n0 = (n / b_count) as usize;
        let mut mask = 0;
        for i in 0..b_count as usize {
            if iy[i * n0..(i + 1) * n0].iter().any(|&v| v != 0) {
                mask |= 1 << i;
            }
        }
        mask
    }

    fn decode_theta(&mut self, qn: i32, n: i32, b0: i32, stereo: bool) -> i32 {
        let rc = &mut *self.rc;
        let itheta;
        if stereo && n > 2 {
            let p0 = 3;
            let x0 = qn / 2;
            let ft = p0 * (x0 + 1) + x0;
            let fs = rc.decode(ft as u32) as i32;
            let x = if fs < (x0 + 1) * p0 { fs / p0 } else { x0 + 1 + (fs - (x0 + 1) * p0) };
            let fl = if x <= x0 { p0 * x } else { (x - 1 - x0) + (x0 + 1) * p0 };
            let fh = if x <= x0 { p0 * (x + 1) } else { (x - x0) + (x0 + 1) * p0 };
            rc.update(fl as u32, fh as u32, ft as u32);
            itheta = x;
        } else if b0 > 1 || stereo {
            itheta = rc.uint(qn as u32 + 1) as i32;
        } else {
            let half = qn >> 1;
            let ft = (half + 1) * (half + 1);
            let fm = rc.decode(ft as u32) as i32;
            let (fs, fl);
            if fm < (half * (half + 1) >> 1) {
                itheta = (isqrt32(8 * fm as u32 + 1) as i32 - 1) >> 1;
                fs = itheta + 1;
                fl = itheta * (itheta + 1) >> 1;
            } else {
                itheta = (2 * (qn + 1) - isqrt32(8 * (ft - fm - 1) as u32 + 1) as i32) >> 1;
                fs = qn + 1 - itheta;
                fl = ft - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1);
            }
            rc.update(fl as u32, (fl + fs) as u32, ft as u32);
        }
        itheta * 16384 / qn
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn quant_band(
        &mut self,
        band: i32,
        x: *mut i32,
        y: *mut i32,
        n: i32,
        b: i32,
        b_count: i32,
        tf_change: i32,
        lowband: *mut i32,
        lm: i32,
        lowband_out: *mut i32,
        level: i32,
        gain: f32,
        lowband_scratch: *mut i32,
        fill: i32,
    ) -> u32 {
        const INTERLEAVE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
        const DEINTERLEAVE: [u8; 16] =
            [0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF];
        let (mut n, mut b, mut b_count, mut tf_change, mut lm, mut fill) = (n, b, b_count, tf_change, lm, fill);
        let (mut y, mut lowband) = (y, lowband);
        let n0 = n;
        let mut n_b = n;
        let long_blocks = b_count == 1;
        let mut b0 = b_count;
        let mut time_divide = 0;
        let mut recombine = 0;
        let mut inv = false;
        let (imid, iside);
        let mut mid = 0f32;
        let side;
        let mut cm = 0u32;
        let stereo = !y.is_null();
        let mut split = stereo;
        let tmp = self.scratch;

        n_b /= b_count;
        let mut n_b0 = n_b;

        unsafe {
            if n == 1 {
                let mut v = x;
                for _ in 0..1 + i32::from(stereo) {
                    let mut sign = 0;
                    if self.remaining_bits >= 1 << BITRES {
                        sign = self.rc.bits(1);
                        self.remaining_bits -= 1 << BITRES;
                    }
                    *v = if sign != 0 { -NORM_ONE } else { NORM_ONE };
                    v = y;
                }
                if !lowband_out.is_null() {
                    *lowband_out = *x;
                }
                return 1;
            }

            if !stereo && level == 0 {
                if tf_change > 0 {
                    recombine = tf_change;
                }
                if !lowband.is_null() && (recombine != 0 || ((n_b & 1) == 0 && tf_change < 0) || b0 > 1) {
                    core::ptr::copy_nonoverlapping(lowband, lowband_scratch, n as usize);
                    lowband = lowband_scratch;
                }
                for k in 0..recombine {
                    if !lowband.is_null() {
                        haar1(lowband, n >> k, 1 << k);
                    }
                    fill = i32::from(INTERLEAVE[(fill & 0xF) as usize]) | i32::from(INTERLEAVE[(fill >> 4) as usize]) << 2;
                }
                b_count >>= recombine;
                n_b <<= recombine;
                while (n_b & 1) == 0 && tf_change < 0 {
                    if !lowband.is_null() {
                        haar1(lowband, n_b, b_count);
                    }
                    fill |= fill << b_count;
                    b_count <<= 1;
                    n_b >>= 1;
                    time_divide += 1;
                    tf_change += 1;
                }
                b0 = b_count;
                n_b0 = n_b;
                if b0 > 1 && !lowband.is_null() {
                    deinterleave_hadamard(lowband, tmp, n_b >> recombine, b0 << recombine, long_blocks);
                }
            }

            let cache = pulse_cache(band, lm);
            if !stereo && lm != -1 && b > i32::from(cache[usize::from(cache[0])]) + 12 && n > 2 {
                n >>= 1;
                y = x.add(n as usize);
                split = true;
                lm -= 1;
                if b_count == 1 {
                    fill = (fill & 1) | (fill << 1);
                }
                b_count = (b_count + 1) >> 1;
            }

            if split {
                let mut itheta = 0;
                let mut delta;
                let pulse_cap = i32::from(CELT_LOG_N[band as usize]) + lm * (1 << BITRES);
                let theta_offset = if stereo && n == 2 { QTHETA_OFFSET_TWOPHASE } else { QTHETA_OFFSET };
                let offset = (pulse_cap >> 1) - theta_offset;
                let mut qn = compute_qn(n, b, offset, pulse_cap, stereo);
                if stereo && band >= self.intensity {
                    qn = 1;
                }
                let tell = self.rc.tell_frac() as i32;
                if qn != 1 {
                    itheta = self.decode_theta(qn, n, b0, stereo);
                } else if stereo && b > 2 << BITRES && self.remaining_bits > 2 << BITRES {
                    inv = self.rc.bit_logp(2);
                }
                let qalloc = self.rc.tell_frac() as i32 - tell;
                b -= qalloc;
                let orig_fill = fill;
                if itheta == 0 {
                    imid = 32767;
                    iside = 0;
                    fill &= (1 << b_count) - 1;
                    delta = -16384;
                } else if itheta == 16384 {
                    imid = 0;
                    iside = 32767;
                    fill &= ((1 << b_count) - 1) << b_count;
                    delta = 16384;
                } else {
                    imid = bitexact_cos(itheta);
                    iside = bitexact_cos(16384 - itheta);
                    delta = frac_mul16((n - 1) << 7, bitexact_log2tan(iside, imid));
                }
                mid = imid as f32 / 32768.0;
                side = iside as f32 / 32768.0;

                if n == 2 && stereo {
                    let sbits = if itheta != 0 && itheta != 16384 { 1 << BITRES } else { 0 };
                    let mbits = b - sbits;
                    let c = itheta > 8192;
                    self.remaining_bits -= qalloc + sbits;
                    let (x2, y2) = if c { (y, x) } else { (x, y) };
                    let mut sign = 0;
                    if sbits != 0 {
                        sign = self.rc.bits(1) as i32;
                    }
                    sign = 1 - 2 * sign;
                    cm = self.quant_band(band, x2, core::ptr::null_mut(), n, mbits, b_count, tf_change, lowband, lm, lowband_out, level, gain, lowband_scratch, orig_fill);
                    *y2 = -sign * *x2.add(1);
                    *y2.add(1) = sign * *x2;
                    let (x0, x1) = (mul_q15(*x, imid), mul_q15(*x.add(1), imid));
                    let (y0, y1) = (mul_q15(*y, iside), mul_q15(*y.add(1), iside));
                    *x = x0 - y0;
                    *y = x0 + y0;
                    *x.add(1) = x1 - y1;
                    *y.add(1) = x1 + y1;
                } else {
                    let mut next_lowband2 = core::ptr::null_mut();
                    let mut next_lowband_out1 = core::ptr::null_mut();
                    let mut next_level = 0;
                    if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                        if itheta > 8192 {
                            delta -= delta >> (4 - lm);
                        } else {
                            delta = (delta + (n << BITRES >> (5 - lm))).min(0);
                        }
                    }
                    let mut mbits = b.min((b - delta) / 2).max(0);
                    let mut sbits = b - mbits;
                    self.remaining_bits -= qalloc;
                    if !lowband.is_null() && !stereo {
                        next_lowband2 = lowband.add(n as usize);
                    }
                    if stereo {
                        next_lowband_out1 = lowband_out;
                    } else {
                        next_level = level + 1;
                    }
                    let mut rebalance = self.remaining_bits;
                    let side_shift = (b0 >> 1) & (i32::from(stereo) - 1);
                    let null = core::ptr::null_mut();
                    let mid_gain = if stereo { 1.0 } else { gain * mid };
                    if mbits >= sbits {
                        cm = self.quant_band(band, x, null, n, mbits, b_count, tf_change, lowband, lm, next_lowband_out1, next_level, mid_gain, lowband_scratch, fill);
                        rebalance = mbits - (rebalance - self.remaining_bits);
                        if rebalance > 3 << BITRES && itheta != 0 {
                            sbits += rebalance - (3 << BITRES);
                        }
                        cm |= self.quant_band(band, y, null, n, sbits, b_count, tf_change, next_lowband2, lm, null, next_level, gain * side, null, fill >> b_count) << side_shift;
                    } else {
                        cm = self.quant_band(band, y, null, n, sbits, b_count, tf_change, next_lowband2, lm, null, next_level, gain * side, null, fill >> b_count) << side_shift;
                        rebalance = sbits - (rebalance - self.remaining_bits);
                        if rebalance > 3 << BITRES && itheta != 16384 {
                            mbits += rebalance - (3 << BITRES);
                        }
                        cm |= self.quant_band(band, x, null, n, mbits, b_count, tf_change, lowband, lm, next_lowband_out1, next_level, mid_gain, lowband_scratch, fill);
                    }
                }
            } else {
                let mut q = bits2pulses(band, lm, b);
                let mut curr_bits = pulses2bits(band, lm, q);
                self.remaining_bits -= curr_bits;
                while self.remaining_bits < 0 && q > 0 {
                    self.remaining_bits += curr_bits;
                    q -= 1;
                    curr_bits = pulses2bits(band, lm, q);
                    self.remaining_bits -= curr_bits;
                }
                if q != 0 {
                    cm = self.pvq_unquant(x, n, get_pulses(q), b_count, gain);
                } else {
                    let cm_mask = (1u32 << b_count) - 1;
                    fill &= cm_mask as i32;
                    if fill == 0 {
                        core::ptr::write_bytes(x, 0, n as usize);
                    } else {
                        if lowband.is_null() {
                            for j in 0..n as usize {
                                self.seed = lcg(self.seed);
                                *x.add(j) = (self.seed as i32) >> 12;
                            }
                            cm = cm_mask;
                        } else {
                            const T: i32 = NORM_ONE / 256;
                            for j in 0..n as usize {
                                self.seed = lcg(self.seed);
                                *x.add(j) = (*lowband.add(j)).wrapping_add(if self.seed & 0x8000 != 0 { T } else { -T });
                            }
                            cm = fill as u32;
                        }
                        renormalise(x, n as usize, gain);
                    }
                }
            }

            if stereo {
                if n != 2 {
                    stereo_merge(x, y, mid, n as usize);
                }
                if inv && self.stereo_inversion {
                    for j in 0..n as usize {
                        *y.add(j) = (*y.add(j)).wrapping_neg();
                    }
                }
            } else if level == 0 {
                if b0 > 1 {
                    interleave_hadamard(x, tmp, n_b >> recombine, b0 << recombine, long_blocks);
                }
                n_b = n_b0;
                b_count = b0;
                for _ in 0..time_divide {
                    b_count >>= 1;
                    n_b <<= 1;
                    cm |= cm >> b_count;
                    haar1(x, n_b, b_count);
                }
                for k in 0..recombine {
                    cm = u32::from(DEINTERLEAVE[cm as usize]);
                    haar1(x, n0 >> k, 1 << k);
                }
                b_count <<= recombine;
                if !lowband_out.is_null() {
                    let scale = sqrtf(n0 as f32);
                    for j in 0..n0 as usize {
                        *lowband_out.add(j) = lrintf(scale * *x.add(j) as f32);
                    }
                }
                cm &= (1u32 << b_count) - 1;
            }
        }
        cm
    }

    #[allow(clippy::too_many_arguments)]
    pub unsafe fn quant_all_bands(
        &mut self,
        start: usize,
        end: usize,
        x_all: *mut i32,
        y_all: *mut i32,
        collapse: &mut [u8],
        pulses: &[i32],
        short_blocks: i32,
        dual_stereo: bool,
        tf_res: &[i32],
        total_bits: i32,
        balance: i32,
        norm: *mut i32,
    ) {
        let lm = self.lm;
        let m = 1 << lm;
        let b_count = if short_blocks != 0 { m } else { 1 };
        let c_count = if y_all.is_null() { 1 } else { 2 };
        let eb = |i: usize| i32::from(CELT_EBANDS[i]);
        let mut balance = balance;
        let mut dual_stereo = dual_stereo;
        unsafe {
            let norm2 = norm.add((m * eb(CELT_BANDS)) as usize);
            let mut lowband_offset = 0usize;
            let mut update_lowband = true;

            for i in start..end {
                let x = x_all.add((m * eb(i)) as usize);
                let y = if y_all.is_null() { y_all } else { y_all.add((m * eb(i)) as usize) };
                let n = m * eb(i + 1) - m * eb(i);
                let tell = self.rc.tell_frac() as i32;
                let mut effective_lowband = -1;
                let (mut x_cm, mut y_cm);

                if i != start {
                    balance -= tell;
                }
                self.remaining_bits = total_bits - tell - 1;
                let b = if (i as i32) <= self.coded_bands - 1 {
                    let curr_balance = balance / (self.coded_bands - i as i32).min(3);
                    (self.remaining_bits + 1).min(pulses[i] + curr_balance).min(16383).max(0)
                } else {
                    0
                };
                if (m * eb(i) - n >= m * eb(start) || i == start + 1) && (update_lowband || lowband_offset == 0) {
                    lowband_offset = i;
                }
                if i == start + 1 {
                    let n1 = m * (eb(start + 1) - eb(start));
                    let n2 = m * (eb(start + 2) - eb(start + 1));
                    let offset = m * eb(start);
                    if n2 > n1 {
                        let count = (n2 - n1) as usize;
                        core::ptr::copy(norm.add((offset + 2 * n1 - n2) as usize), norm.add((offset + n1) as usize), count);
                        if c_count == 2 {
                            core::ptr::copy(norm2.add((offset + 2 * n1 - n2) as usize), norm2.add((offset + n1) as usize), count);
                        }
                    }
                }
                let tf_change = tf_res[i];
                if lowband_offset != 0 && (self.spread != SPREAD_AGGRESSIVE || b_count > 1 || tf_change < 0) {
                    effective_lowband = (m * eb(start)).max(m * eb(lowband_offset) - n);
                    let mut fold_start = lowband_offset - 1;
                    while m * eb(fold_start) > effective_lowband {
                        fold_start -= 1;
                    }
                    let mut fold_end = lowband_offset;
                    while fold_end < i && m * eb(fold_end) < effective_lowband + n {
                        fold_end += 1;
                    }
                    x_cm = 0;
                    y_cm = 0;
                    let mut fold_i = fold_start;
                    loop {
                        x_cm |= u32::from(collapse[fold_i * c_count]);
                        y_cm |= u32::from(collapse[fold_i * c_count + c_count - 1]);
                        fold_i += 1;
                        if fold_i >= fold_end {
                            break;
                        }
                    }
                } else {
                    x_cm = (1u32 << b_count) - 1;
                    y_cm = x_cm;
                }
                if dual_stereo && i as i32 == self.intensity {
                    dual_stereo = false;
                    for j in (m * eb(start)) as usize..(m * eb(i)) as usize {
                        *norm.add(j) = ((i64::from(*norm.add(j)) + i64::from(*norm2.add(j))) >> 1) as i32;
                    }
                }
                let null = core::ptr::null_mut();
                let low = |base: *mut i32| if effective_lowband != -1 { base.add(effective_lowband as usize) } else { null };
                let at = (m * eb(i)) as usize;
                let scratch = self.lowband_scratch;
                if dual_stereo {
                    x_cm = self.quant_band(i as i32, x, null, n, b / 2, b_count, tf_change, low(norm), lm, norm.add(at), 0, 1.0, scratch, x_cm as i32);
                    y_cm = self.quant_band(i as i32, y, null, n, b / 2, b_count, tf_change, low(norm2), lm, norm2.add(at), 0, 1.0, scratch, y_cm as i32);
                } else {
                    x_cm = self.quant_band(i as i32, x, y, n, b, b_count, tf_change, low(norm), lm, norm.add(at), 0, 1.0, scratch, (x_cm | y_cm) as i32);
                    y_cm = x_cm;
                }
                collapse[i * c_count] = x_cm as u8;
                collapse[i * c_count + c_count - 1] = y_cm as u8;
                balance += pulses[i] + tell;
                update_lowband = b > (n << BITRES);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn anti_collapse(
    x_all: *mut i32,
    collapse: &[u8],
    lm: i32,
    c_count: usize,
    size: usize,
    start: usize,
    end: usize,
    log_e: &[i32],
    prev1: &[i32],
    prev2: &[i32],
    pulses: &[i32],
    seed: u32,
) {
    let mut seed = seed;
    for i in start..end {
        let n0 = i32::from(CELT_EBANDS[i + 1]) - i32::from(CELT_EBANDS[i]);
        let depth = (1 + pulses[i]) / (n0 << lm);
        let thresh = 0.5 * exp2(-0.125 * depth as f32);
        let sqrt_1 = 1.0 / sqrtf((n0 << lm) as f32);
        for c in 0..c_count {
            let mut p1 = prev1[c * CELT_BANDS + i];
            let mut p2 = prev2[c * CELT_BANDS + i];
            if c_count == 1 {
                p1 = p1.max(prev1[CELT_BANDS + i]);
                p2 = p2.max(prev2[CELT_BANDS + i]);
            }
            let ediff = (log_e[c * CELT_BANDS + i] - p1.min(p2)).max(0);
            let mut r = 2.0 * exp2(-(ediff as f32) / 1024.0);
            if lm == 3 {
                r *= 1.41421356;
            }
            if r > thresh {
                r = thresh;
            }
            let v = lrintf(r * sqrt_1 * NORM_ONE as f32);
            unsafe {
                let x = x_all.add(c * size + ((CELT_EBANDS[i] as usize) << lm));
                let mut renormalize = false;
                for k in 0..1usize << lm {
                    if u32::from(collapse[i * c_count + c]) & (1 << k) == 0 {
                        for j in 0..n0 as usize {
                            seed = lcg(seed);
                            *x.add((j << lm) + k) = if seed & 0x8000 != 0 { v } else { -v };
                        }
                        renormalize = true;
                    }
                }
                if renormalize {
                    renormalise(x, (n0 << lm) as usize, 1.0);
                }
            }
        }
    }
}
