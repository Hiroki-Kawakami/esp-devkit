/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::mem::MaybeUninit;

use devkit::Zeroable;
use devkit::math::{lrintf, sqrtf};

use super::sbr::{Ch, HF_ADJ, MAX_BANDS, MAX_ENV, Sbr, XLOW_SLOTS};

#[repr(C)]
pub(super) struct Hf {
    coef: [[f32; 4]; MAX_BANDS],
    src: [i8; MAX_BANDS],
    e_orig: [[f32; MAX_BANDS]; MAX_ENV],
    q_mapped: [[f32; MAX_BANDS]; MAX_ENV],
    s_mapped: [[u8; MAX_BANDS]; MAX_ENV],
    s_index: [[u8; MAX_BANDS]; MAX_ENV + 1],
    e_curr: [[f32; MAX_BANDS]; MAX_ENV],
    gain: [[f32; MAX_BANDS]; MAX_ENV],
    q_m: [[f32; MAX_BANDS]; MAX_ENV],
    s_m: [[f32; MAX_BANDS]; MAX_ENV],
    g_steady: [[f32; MAX_BANDS]; MAX_ENV],
    q_steady: [[f32; MAX_BANDS]; MAX_ENV],
    alpha: [[f32; 4]; 32],
    env_of_slot: [i8; 2 * 19],
}

unsafe impl Zeroable for Hf {}

fn lpc(s: &Sbr, x_low: *const i32, w: &mut Hf) {
    for k in 0..s.k0 as usize {
        let mut re = [const { MaybeUninit::<f32>::uninit() }; XLOW_SLOTS];
        let mut im = [const { MaybeUninit::<f32>::uninit() }; XLOW_SLOTS];
        for n in 0..XLOW_SLOTS {
            unsafe {
                re[n].write(*x_low.add(n * 64 + 2 * k) as f32);
                im[n].write(*x_low.add(n * 64 + 2 * k + 1) as f32);
            }
        }
        let re = unsafe { &*re.as_ptr().cast::<[f32; XLOW_SLOTS]>() };
        let im = unsafe { &*im.as_ptr().cast::<[f32; XLOW_SLOTS]>() };
        let (mut r02r, mut r02i, mut r11, mut r12r, mut r12i) = (0f32, 0f32, 0f32, 0f32, 0f32);
        for n in 1..38 {
            r11 += re[n] * re[n] + im[n] * im[n];
            r12r += re[n + 1] * re[n] + im[n + 1] * im[n];
            r12i += im[n + 1] * re[n] - re[n + 1] * im[n];
        }
        let r22 = r11 + (re[0] * re[0] + im[0] * im[0]);
        r11 += re[38] * re[38] + im[38] * im[38];
        let r01r = r12r + (re[39] * re[38] + im[39] * im[38]);
        let r01i = r12i + (im[39] * re[38] - re[39] * im[38]);
        r12r += re[1] * re[0] + im[1] * im[0];
        r12i += im[1] * re[0] - re[1] * im[0];
        for n in 0..38 {
            r02r += re[n + 2] * re[n] + im[n + 2] * im[n];
            r02i += im[n + 2] * re[n] - re[n + 2] * im[n];
        }
        let (mut a1r, mut a1i, mut a0r, mut a0i) = (0f32, 0f32, 0f32, 0f32);
        let dk = r22 * r11 - (r12r * r12r + r12i * r12i) / 1.000001;
        if dk != 0.0 {
            a1r = (r01r * r12r - r01i * r12i - r02r * r11) / dk;
            a1i = (r01r * r12i + r01i * r12r - r02i * r11) / dk;
        }
        if r11 != 0.0 {
            a0r = -(r01r + a1r * r12r + a1i * r12i) / r11;
            a0i = -(r01i + a1i * r12r - a1r * r12i) / r11;
        }
        if a1r * a1r + a1i * a1i >= 16.0 || a0r * a0r + a0i * a0i >= 16.0 {
            a1r = 0.0;
            a1i = 0.0;
            a0r = 0.0;
            a0i = 0.0;
        }
        w.alpha[k] = [a0r, a0i, a1r, a1i];
    }
}

#[optimize(size)]
#[inline(never)]
fn chirp(s: &Sbr, c: &mut Ch) {
    const TAB: [f32; 4] = [0.0, 0.75, 0.9, 0.98];
    for i in 0..s.n_q as usize {
        let mut bw = if c.invf[0][i] + c.invf[1][i] == 1 { 0.6 } else { TAB[usize::from(c.invf[0][i])] };
        if bw < c.bw[i] {
            bw = 0.75 * bw + 0.25 * c.bw[i];
        } else {
            bw = 0.90625 * bw + 0.09375 * c.bw[i];
        }
        c.bw[i] = if bw < 0.015625 { 0.0 } else { bw };
    }
}

#[optimize(size)]
#[inline(never)]
fn patch_coefficients(s: &Sbr, c: &Ch, w: &mut Hf) {
    w.src.fill(-1);
    let mut k = s.kx;
    let mut g = 0i32;
    for j in 0..s.num_patches as usize {
        for x in 0..i32::from(s.patch_num[j]) {
            let p = i32::from(s.patch_start[j]) + x;
            while g <= s.n_q && k >= i32::from(s.f_noise[g as usize]) {
                g += 1;
            }
            g = (g - 1).max(0);
            let bw = c.bw[g as usize];
            let m = (k - s.kx) as usize;
            let a = &w.alpha[p as usize];
            w.coef[m] = [a[0] * bw, a[1] * bw, a[2] * bw * bw, a[3] * bw * bw];
            w.src[m] = p as i8;
            k += 1;
        }
    }
}

#[inline(always)]
fn x_high(w: &Hf, x_low: *const i32, m: usize, idx: usize) -> (f32, f32) {
    debug_assert!(m < MAX_BANDS);
    let p = unsafe { *w.src.get_unchecked(m) };
    if p < 0 {
        return (0.0, 0.0);
    }
    unsafe {
        let co = w.coef.get_unchecked(m);
        let x0 = x_low.add(idx * 64 + 2 * p as usize);
        let (r0, i0) = (*x0 as f32, *x0.add(1) as f32);
        let (r1, i1) = (*x0.sub(64) as f32, *x0.sub(63) as f32);
        let (r2, i2) = (*x0.sub(128) as f32, *x0.sub(127) as f32);
        (
            r2 * co[2] - i2 * co[3] + r1 * co[0] - i1 * co[1] + r0,
            i2 * co[2] + r2 * co[3] + i1 * co[0] + r1 * co[1] + i0,
        )
    }
}

#[optimize(size)]
#[inline(never)]
fn mapping(s: &Sbr, c: &mut Ch, w: &mut Hf) {
    let kx = s.kx as usize;
    w.s_index = [[0; MAX_BANDS]; MAX_ENV + 1];
    w.s_index[0] = c.s_index_prev;
    for e in 0..usize::from(c.num_env) {
        let fr = c.freq_res[e + 1] != 0;
        let (table, n): (&[u8], i32) = if fr { (&s.f_high, s.n_high) } else { (&s.f_low, s.n_low) };
        let n = n as usize;
        for i in 0..n {
            w.e_orig[e][usize::from(table[i]) - kx..usize::from(table[i + 1]) - kx].fill(c.env[e][i]);
        }
        let q = usize::from(c.num_noise > 1 && c.t_env[e] >= c.t_q[1]);
        for i in 0..s.n_q as usize {
            w.q_mapped[e][usize::from(s.f_noise[i]) - kx..usize::from(s.f_noise[i + 1]) - kx].fill(c.noise[q][i]);
        }
        if c.add_harmonic_flag {
            for i in 0..s.n_high as usize {
                let mid = ((usize::from(s.f_high[i]) + usize::from(s.f_high[i + 1])) >> 1) - kx;
                let on = c.add_harmonic[i] != 0 && (e as i32 >= i32::from(c.l_a[1]) || w.s_index[0][mid] == 1);
                w.s_index[e + 1][mid] = u8::from(on);
            }
        }
        if !c.add_harmonic_flag {
            w.s_mapped[e] = [0; MAX_BANDS];
            continue;
        }
        for i in 0..n {
            let (lo, hi) = (usize::from(table[i]) - kx, usize::from(table[i + 1]) - kx);
            let present = w.s_index[e + 1][lo..hi].iter().fold(0, |p, &v| p | v);
            w.s_mapped[e][lo..hi].fill(present);
        }
    }
    c.s_index_prev = w.s_index[usize::from(c.num_env)];
}

// One band of `x_high` energy over slots `ilb..iub`, added to `sum`. The
// source and coefficients stay out of the slot loop, which is the hot one.
#[inline(always)]
fn add_energy(w: &Hf, x_low: *const i32, m: usize, ilb: usize, iub: usize, sum: f32) -> f32 {
    let p = w.src[m];
    if p < 0 {
        return sum;
    }
    let [c0, c1, c2, c3] = w.coef[m];
    let mut sum = sum;
    unsafe {
        let mut x0 = x_low.add(ilb * 64 + 2 * p as usize);
        let (mut r1, mut i1) = (*x0.sub(64) as f32, *x0.sub(63) as f32);
        let (mut r2, mut i2) = (*x0.sub(128) as f32, *x0.sub(127) as f32);
        for _ in ilb..iub {
            let (r0, i0) = (*x0 as f32, *x0.add(1) as f32);
            let re = r2 * c2 - i2 * c3 + r1 * c0 - i1 * c1 + r0;
            let im = i2 * c2 + r2 * c3 + i1 * c0 + r1 * c1 + i0;
            sum += re * re + im * im;
            (r2, i2, r1, i1) = (r1, i1, r0, i0);
            x0 = x0.add(64);
        }
    }
    sum
}

fn estimate(s: &Sbr, c: &Ch, w: &mut Hf) {
    let kx = s.kx as usize;
    let m_max = s.m as usize;
    assert!(m_max <= MAX_BANDS);
    for e in 0..usize::from(c.num_env) {
        let ilb = 2 * usize::from(c.t_env[e]) + HF_ADJ;
        let iub = 2 * usize::from(c.t_env[e + 1]) + HF_ADJ;
        if s.interpol_freq != 0 {
            let recip = 1.0 / (iub - ilb) as f32;
            for m in 0..m_max {
                w.e_curr[e][m] = add_energy(w, c.x_low, m, ilb, iub, 0.0) * recip;
            }
        } else {
            let fr = c.freq_res[e + 1] != 0;
            let (table, n): (&[u8], i32) = if fr { (&s.f_high, s.n_high) } else { (&s.f_low, s.n_low) };
            for p in 0..n as usize {
                let (lo, hi) = (usize::from(table[p]), usize::from(table[p + 1]));
                let mut sum = 0f32;
                for k in lo..hi {
                    sum = add_energy(w, c.x_low, k - kx, ilb, iub, sum);
                }
                sum /= ((iub - ilb) * (hi - lo)) as f32;
                w.e_curr[e][lo - kx..hi - kx].fill(sum);
            }
        }
    }
}

#[optimize(size)]
#[inline(never)]
fn gains(s: &Sbr, c: &Ch, w: &mut Hf) {
    const LIMGAIN: [f32; 4] = [0.70795, 1.0, 1.41254, 10000000000.0];
    let kx = s.kx as usize;
    let num_env = usize::from(c.num_env);
    assert!(num_env <= MAX_ENV);
    let limgain = LIMGAIN[s.limiter_gains as usize];
    for e in 0..num_env {
        let delta = !(e as i32 == i32::from(c.l_a[1]) || e as i32 == i32::from(c.l_a[0]));
        let fdelta = if delta { 1.0f32 } else { 0.0 };
        let (e_orig, q_mapped, e_curr) = (&w.e_orig[e], &w.q_mapped[e], &w.e_curr[e]);
        let (s_index, s_mapped) = (&w.s_index[e + 1], &w.s_mapped[e]);
        let (q_m, s_m, gain) = (&mut w.q_m[e], &mut w.s_m[e], &mut w.gain[e]);
        for k in 0..s.n_lim as usize {
            let (m0, m1) = (usize::from(s.f_lim[k]) - kx, usize::from(s.f_lim[k + 1]) - kx);
            assert!(m0 <= m1 && m1 <= MAX_BANDS);
            for m in m0..m1 {
                let temp = e_orig[m] / (1.0 + q_mapped[m]);
                q_m[m] = sqrtf(temp * q_mapped[m]);
                s_m[m] = sqrtf(temp * f32::from(s_index[m]));
                if s_mapped[m] == 0 {
                    gain[m] = sqrtf(e_orig[m] / ((1.0 + e_curr[m]) * (1.0 + q_mapped[m] * fdelta)));
                } else {
                    gain[m] = sqrtf(e_orig[m] * q_mapped[m] / ((1.0 + e_curr[m]) * (1.0 + q_mapped[m])));
                }
                gain[m] += f32::MIN_POSITIVE;
            }
            let (mut sum0, mut sum1) = (0f32, 0f32);
            for m in m0..m1 {
                sum0 += e_orig[m];
                sum1 += e_curr[m];
            }
            let mut gain_max = limgain * sqrtf((f32::EPSILON + sum0) / (f32::EPSILON + sum1));
            if gain_max > 100000.0 {
                gain_max = 100000.0;
            }
            for m in m0..m1 {
                let q_max = q_m[m] * gain_max / gain[m];
                if q_m[m] > q_max {
                    q_m[m] = q_max;
                }
                if gain[m] > gain_max {
                    gain[m] = gain_max;
                }
            }
            sum0 = 0.0;
            sum1 = 0.0;
            for m in m0..m1 {
                sum0 += e_orig[m];
                let noise = if delta && s_m[m] == 0.0 { 1.0f32 } else { 0.0 };
                sum1 += e_curr[m] * gain[m] * gain[m] + s_m[m] * s_m[m] + noise * q_m[m] * q_m[m];
            }
            let mut boost = sqrtf((f32::EPSILON + sum0) / (f32::EPSILON + sum1));
            if boost > 1.584893192 {
                boost = 1.584893192;
            }
            for m in m0..m1 {
                gain[m] *= boost;
                q_m[m] *= boost;
                s_m[m] *= boost;
            }
        }
    }
}

#[inline(always)]
pub(super) fn to_fixed(v: f32) -> i32 {
    if v > 1073741824.0 {
        1073741824
    } else if v < -1073741824.0 {
        -1073741824
    } else {
        lrintf(v)
    }
}

const H_SMOOTH: [f32; 5] = [0.33333333333333, 0.30150283239582, 0.21816949906249, 0.11516383427084, 0.03183050093751];

fn gain_row<'a>(s: &Sbr, c: &'a Ch, w: &'a Hf, slot: i32, q: bool) -> &'a [f32; MAX_BANDS] {
    let t0 = 2 * i32::from(c.t_env[0]);
    if slot >= t0 {
        let e = w.env_of_slot[slot as usize] as usize;
        return if q { &w.q_m[e] } else { &w.gain[e] };
    }
    if s.reset {
        return if q { &w.q_m[0] } else { &w.gain[0] };
    }
    let at = (slot - (t0 - 4)) as usize;
    if q { &c.q_hist[at] } else { &c.g_hist[at] }
}

pub(super) fn prepare(s: &Sbr, c: &mut Ch, w: &mut Hf) {
    lpc(s, c.x_low, w);
    chirp(s, c);
    patch_coefficients(s, c, w);
    mapping(s, c, w);
    estimate(s, c, w);
    gains(s, c, w);
    for e in 0..usize::from(c.num_env) {
        let (lo, hi) = (2 * usize::from(c.t_env[e]), 2 * usize::from(c.t_env[e + 1]));
        w.env_of_slot[lo..hi].fill(e as i8);
        for m in 0..s.m as usize {
            let (mut gs, mut qs) = (0f32, 0f32);
            for h in &H_SMOOTH {
                gs += w.gain[e][m] * h;
                qs += w.q_m[e][m] * h;
            }
            w.g_steady[e][m] = gs;
            w.q_steady[e][m] = qs;
        }
    }
}

/// Writes the high band of slot `i` into `row`, or only advances the noise and
/// sinusoid phase when `row` is null; slots are visited in order.
pub(super) fn slot(s: &Sbr, c: &mut Ch, w: &Hf, i: i32, row: *mut i32) {
    if row.is_null() {
        c.noise_index = (usize::from(c.noise_index) + s.m as usize) as u16 & 0x1ff;
        c.sine_index = (c.sine_index + 1) & 3;
        return;
    }
    const PHI_RE: [f32; 4] = [1.0, 0.0, -1.0, 0.0];
    const PHI_IM: [f32; 4] = [0.0, 1.0, 0.0, -1.0];
    let h_sl = if s.smoothing_mode != 0 { 0 } else { 4 };
    let kx = s.kx as usize;
    let m_max = s.m as usize;
    let e = w.env_of_slot[i as usize] as usize;
    let transient = e as i32 == i32::from(c.l_a[0]) || e as i32 == i32::from(c.l_a[1]);
    let (mut g_filt_buf, mut q_filt_buf);
    let (g_filt, q_filt);
    if h_sl != 0 && !transient && i - h_sl >= 2 * i32::from(c.t_env[e]) {
        g_filt = &w.g_steady[e];
        q_filt = &w.q_steady[e];
    } else if h_sl != 0 && !transient {
        g_filt_buf = [0f32; MAX_BANDS];
        q_filt_buf = [0f32; MAX_BANDS];
        for j in 0..=h_sl {
            let g = gain_row(s, c, w, i - j, false);
            let q = gain_row(s, c, w, i - j, true);
            let h = H_SMOOTH[j as usize];
            for m in 0..m_max {
                g_filt_buf[m] += g[m] * h;
                q_filt_buf[m] += q[m] * h;
            }
        }
        g_filt = &g_filt_buf;
        q_filt = &q_filt_buf;
    } else {
        g_filt = &w.gain[e];
        q_filt = &w.q_m[e];
    }
    let mut noise = usize::from(c.noise_index);
    let sine = usize::from(c.sine_index) & 3;
    assert!(m_max <= MAX_BANDS);
    let s_m = &w.s_m[e];
    for m in 0..m_max {
        let (xr, xi) = x_high(w, c.x_low, m, i as usize + HF_ADJ);
        let mut re = xr * g_filt[m];
        let mut im = xi * g_filt[m];
        noise = (noise + 1) & 0x1ff;
        let sm = s_m[m];
        if sm != 0.0 {
            re += sm * PHI_RE[sine];
            im += if (kx + m) & 1 != 0 { -sm * PHI_IM[sine] } else { sm * PHI_IM[sine] };
        } else if !transient {
            re += q_filt[m] * s.noise_table[noise][0];
            im += q_filt[m] * s.noise_table[noise][1];
        }
        unsafe {
            *row.add(2 * (kx + m)) = to_fixed(re);
            *row.add(2 * (kx + m) + 1) = to_fixed(im);
        }
    }
    c.noise_index = noise as u16;
    c.sine_index = ((sine + 1) & 3) as u8;
}

pub(super) fn finish(s: &Sbr, c: &mut Ch, w: &Hf) {
    if s.smoothing_mode != 0 {
        return;
    }
    let end = 2 * i32::from(c.t_env[usize::from(c.num_env)]);
    let mut g = [[0f32; MAX_BANDS]; 4];
    let mut q = [[0f32; MAX_BANDS]; 4];
    for i in 0..4 {
        g[i] = *gain_row(s, c, w, end - 4 + i as i32, false);
        q[i] = *gain_row(s, c, w, end - 4 + i as i32, true);
    }
    c.g_hist = g;
    c.q_hist = q;
}
