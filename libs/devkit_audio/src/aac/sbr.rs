/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit::math::{floor, ldexpf, log2, pow};
use devkit::{A16, Zeroable};
use idf::heap::{self, Caps};

use super::filterbank::filterbank;
use super::kernels::{Tables, dct4_64x4, pack, qmf_analysis, qmf_synthesis, split, syn_in4, syn_v4};
use super::ps::Ps;
use super::sbr_hf::{self, Hf};
use super::tables::*;
use super::{Dec, FRAME_LEN, ID_SCE, ProfileStage, huff};
use crate::bits::Bits;
use crate::{Error, Result};

pub(super) const SLOTS: usize = 32;
pub(super) const XLOW_SLOTS: usize = 40;
pub(super) const HF_ADJ: usize = 2;
const HF_GEN: usize = 8;
pub(super) const MAX_ENV: usize = 5;
pub(super) const MAX_BANDS: usize = 48;
const MAX_NQ: usize = 5;
const V_LEN: usize = 2560;
const ANA_LEN: usize = 1312;

const SBR_H_T_ENV_1_5: usize = 12;
const SBR_H_F_ENV_1_5: usize = 13;
const SBR_H_T_ENV_BAL_1_5: usize = 14;
const SBR_H_F_ENV_BAL_1_5: usize = 15;
const SBR_H_T_ENV_3_0: usize = 16;
const SBR_H_F_ENV_3_0: usize = 17;
const SBR_H_T_ENV_BAL_3_0: usize = 18;
const SBR_H_F_ENV_BAL_3_0: usize = 19;
const SBR_H_T_NOISE_3_0: usize = 20;
const SBR_H_T_NOISE_BAL_3_0: usize = 21;

const FIXFIX: u8 = 0;
const FIXVAR: u8 = 1;
const VARFIX: u8 = 2;

#[repr(C)]
pub(super) struct Ch {
    frame_class: u8,
    pub num_env: u8,
    pub num_noise: u8,
    amp_res: u8,
    pub freq_res: [u8; MAX_ENV + 1],
    pub t_env: [u8; MAX_ENV + 1],
    pub t_q: [u8; 3],
    t_env_prev_end: u8,
    pub l_a: [i8; 2],
    df_env: [u8; MAX_ENV],
    df_noise: [u8; 2],
    pub invf: [[u8; MAX_NQ]; 2],
    env_q: [[i16; MAX_BANDS]; MAX_ENV + 1],
    noise_q: [[i16; MAX_NQ]; 3],
    pub add_harmonic_flag: bool,
    pub add_harmonic: [u8; MAX_BANDS],
    pub s_index_prev: [u8; MAX_BANDS],
    pub env: [[f32; MAX_BANDS]; MAX_ENV],
    pub noise: [[f32; MAX_NQ]; 2],
    pub bw: [f32; MAX_NQ],
    pub g_hist: [[f32; MAX_BANDS]; 4],
    pub q_hist: [[f32; MAX_BANDS]; 4],
    pub noise_index: u16,
    pub sine_index: u8,
    ana_hi: *mut i16,
    ana_lo: *mut i16,
    pub x_low: *mut i32,
    y_tail: *mut i32,
    v_hi: *mut i16,
    v_lo: *mut i16,
    v_off: usize,
}

unsafe impl Zeroable for Ch {}

#[repr(C)]
pub(super) struct Sbr {
    rate: u32,
    start: bool,
    pub reset: bool,
    have_data: bool,
    coupling: bool,
    amp_res_header: u8,
    start_freq: i32,
    stop_freq: i32,
    xover: i32,
    freq_scale: i32,
    alter_scale: i32,
    noise_bands: i32,
    limiter_bands: i32,
    pub limiter_gains: i32,
    pub interpol_freq: i32,
    pub smoothing_mode: i32,
    pub k0: i32,
    k2: i32,
    pub kx: i32,
    pub m: i32,
    kx_prev: i32,
    m_prev: i32,
    n_master: i32,
    pub n_high: i32,
    pub n_low: i32,
    pub n_q: i32,
    pub n_lim: i32,
    f_master: [u8; 64],
    pub f_high: [u8; MAX_BANDS + 1],
    pub f_low: [u8; MAX_BANDS + 1],
    pub f_noise: [u8; MAX_NQ + 1],
    pub f_lim: [u8; MAX_BANDS + 8],
    pub num_patches: i32,
    pub patch_num: [u8; 6],
    pub patch_start: [u8; 6],
    // A copy: the noise walk visits the whole table every frame and each line
    // missed in flash costs far more than one in the decoder's own memory.
    pub noise_table: [[f32; 2]; 512],
}

unsafe impl Zeroable for Sbr {}

pub(super) struct State {
    s: Box<Sbr, Caps>,
    ch: Box<[Ch; 2], Caps>,
    hf: [Option<Box<Hf, Caps>>; 2],
    ps: Option<Box<Ps, Caps>>,
    _ana: [Option<Box<A16<[i16; 2 * ANA_LEN]>, Caps>>; 2],
    _x_low: [Option<Box<[i32; XLOW_SLOTS * 64], Caps>>; 2],
    _y_tail: [Option<Box<[i32; 6 * 128], Caps>>; 2],
    _v: [Option<Box<A16<[i16; 2 * V_LEN]>, Caps>>; 2],
}

fn turn_off(s: &mut Sbr, ch: &mut [Ch; 2]) {
    s.start = false;
    s.have_data = false;
    s.kx = 32;
    s.m = 0;
    ch[0].l_a[1] = -1;
    ch[1].l_a[1] = -1;
    s.start_freq = -1;
    s.stop_freq = -1;
    s.xover = -1;
    s.freq_scale = -1;
    s.alter_scale = -1;
    s.noise_bands = -1;
}

fn nint(v: f64) -> i32 {
    floor(v + 0.5) as i32
}

fn make_bands(dk: &mut [u8], start: i32, stop: i32, bands: i32) {
    let mut prev = start;
    for k in 0..bands {
        let cur = nint(f64::from(start) * pow(f64::from(stop) / f64::from(start), f64::from(k + 1) / f64::from(bands)));
        dk[k as usize] = (cur - prev) as u8;
        prev = cur;
    }
}

fn make_master(s: &mut Sbr) -> bool {
    let row = match s.rate {
        16000 => 0,
        22050 => 1,
        24000 => 2,
        32000 => 3,
        44100 | 48000 | 64000 => 4,
        88200 | 96000 => 5,
        _ => return false,
    };
    let temp: u32 = match s.rate {
        ..32000 => 3000,
        ..64000 => 4000,
        _ => 5000,
    };
    let start_min = ((temp * 128 + s.rate / 2) / s.rate) as i32;
    let stop_min = ((temp * 256 + s.rate / 2) / s.rate) as i32;
    let k0 = start_min + i32::from(SBR_START_OFFSET[row][s.start_freq as usize]);
    let mut k2;
    if s.stop_freq < 14 {
        let mut dk = [0u8; 13];
        make_bands(&mut dk, stop_min, 64, 13);
        dk.sort_unstable();
        k2 = stop_min;
        for k in 0..s.stop_freq as usize {
            k2 += i32::from(dk[k]);
        }
    } else {
        k2 = (if s.stop_freq == 14 { 2 } else { 3 }) * k0;
    }
    k2 = k2.min(64);
    let max_bands = match s.rate {
        ..=32000 => 48,
        44100 => 35,
        _ => 32,
    };
    if k2 - k0 > max_bands || k0 >= k2 {
        return false;
    }
    s.k0 = k0;
    s.k2 = k2;

    if s.freq_scale == 0 {
        let dk = s.alter_scale + 1;
        let n = ((k2 - k0 + (dk & 2)) >> dk) << 1;
        if n <= 0 || n > 63 {
            return false;
        }
        let mut v = [dk as u8; 64];
        let mut diff = k2 - (k0 + n * dk);
        if diff < 0 {
            let mut k = 0;
            while diff < 0 && k < n {
                v[k as usize] = v[k as usize].wrapping_sub(1);
                k += 1;
                diff += 1;
            }
        } else {
            let mut k = n - 1;
            while diff > 0 && k >= 0 {
                v[k as usize] = v[k as usize].wrapping_add(1);
                k -= 1;
                diff -= 1;
            }
        }
        s.f_master[0] = k0 as u8;
        for k in 1..=n as usize {
            s.f_master[k] = s.f_master[k - 1].wrapping_add(v[k - 1]);
        }
        s.n_master = n;
    } else {
        const BANDS_TAB: [f64; 3] = [12.0, 10.0, 8.0];
        let bands = BANDS_TAB[s.freq_scale as usize - 1];
        let two = 49 * k2 > 110 * k0;
        let k1 = if two { 2 * k0 } else { k2 };
        let n0 = 2 * nint(bands * log2(f64::from(k1) / f64::from(k0)) / 2.0);
        if n0 <= 0 || n0 > 48 {
            return false;
        }
        let mut dk0 = [0u8; 48];
        make_bands(&mut dk0, k0, k1, n0);
        dk0[..n0 as usize].sort_unstable();
        s.f_master[0] = k0 as u8;
        for k in 1..=n0 as usize {
            if dk0[k - 1] == 0 {
                return false;
            }
            s.f_master[k] = s.f_master[k - 1].wrapping_add(dk0[k - 1]);
        }
        let mut n = n0;
        if two {
            let warp = if s.alter_scale != 0 { 1.3 } else { 1.0 };
            let n1 = 2 * nint(bands * log2(f64::from(k2) / f64::from(k1)) / (2.0 * warp));
            if n1 <= 0 || n0 + n1 > 63 {
                return false;
            }
            let mut dk1 = [0u8; 48];
            make_bands(&mut dk1, k1, k2, n1);
            let d1 = &mut dk1[..n1 as usize];
            d1.sort_unstable();
            let last = n1 as usize - 1;
            if d1[0] < dk0[n0 as usize - 1] {
                let mut change = i32::from(dk0[n0 as usize - 1]) - i32::from(d1[0]);
                let limit = (i32::from(d1[last]) - i32::from(d1[0])) >> 1;
                change = change.min(limit);
                d1[0] = (i32::from(d1[0]) + change) as u8;
                d1[last] = (i32::from(d1[last]) - change) as u8;
                d1.sort_unstable();
            }
            for k in 1..=n1 as usize {
                if d1[k - 1] == 0 {
                    return false;
                }
                let at = n0 as usize + k;
                s.f_master[at] = s.f_master[at - 1].wrapping_add(d1[k - 1]);
            }
            n += n1;
        }
        s.n_master = n;
    }
    s.xover < s.n_master
}

fn make_limiter(s: &mut Sbr) {
    if s.limiter_bands == 0 {
        s.f_lim[0] = s.f_low[0];
        s.f_lim[1] = s.f_low[s.n_low as usize];
        s.n_lim = 1;
        return;
    }
    const WARPED: [f64; 3] = [1.32715174233856803909, 1.18509277094158210129, 1.11987160404675912501];
    let ratio = WARPED[s.limiter_bands as usize - 1];
    let num_patches = s.num_patches as usize;
    let mut borders = [0u8; 7];
    borders[0] = s.kx as u8;
    for k in 1..=num_patches {
        borders[k] = borders[k - 1].wrapping_add(s.patch_num[k - 1]);
    }
    let borders = &borders[..num_patches + 1];
    let mut t = [0u8; MAX_BANDS + 8];
    let mut n = s.n_low as usize + 1;
    t[..n].copy_from_slice(&s.f_low[..n]);
    for k in 1..num_patches {
        t[n] = borders[k];
        n += 1;
    }
    t[..n].sort_unstable();
    let mut k = 1;
    while k < n {
        if f64::from(t[k]) >= f64::from(t[k - 1]) * ratio {
            k += 1;
        } else if t[k] == t[k - 1] || !borders.contains(&t[k]) {
            t.copy_within(k + 1..n, k);
            n -= 1;
        } else if !borders.contains(&t[k - 1]) {
            t.copy_within(k..n, k - 1);
            n -= 1;
        } else {
            k += 1;
        }
    }
    s.f_lim[..n].copy_from_slice(&t[..n]);
    s.n_lim = n as i32 - 1;
}

fn make_patches(s: &mut Sbr) -> bool {
    let goal = (((1000u32 << 11) + s.rate / 2) / s.rate) as i32;
    let mut msb = s.k0;
    let mut usb = s.kx;
    let mut sb = 0i32;
    let mut k = if goal < s.kx + s.m {
        s.f_master.iter().position(|&f| i32::from(f) >= goal).unwrap_or(s.f_master.len()) as i32
    } else {
        s.n_master
    };
    s.num_patches = 0;
    let (mut last_k, mut last_msb) = (-1, -1);
    loop {
        if k == last_k && msb == last_msb {
            return false;
        }
        last_k = k;
        last_msb = msb;
        let mut odd = 0;
        let mut j = k;
        while j == k || sb > s.k0 - 1 + msb - odd {
            if j < 0 {
                return false;
            }
            sb = i32::from(s.f_master[j as usize]);
            odd = (sb + s.k0) & 1;
            j -= 1;
        }
        if s.num_patches > 5 {
            return false;
        }
        let num = (sb - usb).max(0);
        s.patch_num[s.num_patches as usize] = num as u8;
        s.patch_start[s.num_patches as usize] = (s.k0 - odd - num) as u8;
        if num > 0 {
            usb = sb;
            msb = sb;
            s.num_patches += 1;
        } else {
            msb = s.kx;
        }
        if i32::from(s.f_master[k as usize]) - sb < 3 {
            k = s.n_master;
        }
        if sb == s.kx + s.m {
            break;
        }
    }
    if s.num_patches > 1 && s.patch_num[s.num_patches as usize - 1] < 3 {
        s.num_patches -= 1;
    }
    for j in 0..s.num_patches as usize {
        if u32::from(s.patch_start[j]) + u32::from(s.patch_num[j]) > 32 {
            return false;
        }
    }
    s.num_patches > 0
}

fn make_tables(s: &mut Sbr, ch: &mut [Ch; 2]) -> bool {
    if !make_master(s) {
        return false;
    }
    s.n_high = s.n_master - s.xover;
    s.n_low = (s.n_high + 1) >> 1;
    let (n_high, xover) = (s.n_high as usize, s.xover as usize);
    s.f_high[..n_high + 1].copy_from_slice(&s.f_master[xover..xover + n_high + 1]);
    s.kx = i32::from(s.f_high[0]);
    s.m = i32::from(s.f_high[n_high].wrapping_sub(s.f_high[0]));
    if s.kx + s.m > 64 || s.kx > 32 || s.m > MAX_BANDS as i32 {
        return false;
    }
    s.f_low[0] = s.f_high[0];
    let odd = n_high & 1;
    for k in 1..=s.n_low as usize {
        s.f_low[k] = s.f_high[2 * k - odd];
    }
    let nq = nint(f64::from(s.noise_bands) * log2(f64::from(s.k2) / f64::from(s.kx))).max(1);
    if nq > MAX_NQ as i32 {
        return false;
    }
    s.n_q = nq;
    s.f_noise[0] = s.f_low[0];
    let mut t = 0;
    for k in 1..=s.n_q {
        t += (s.n_low - t) / (s.n_q + 1 - k);
        s.f_noise[k as usize] = s.f_low[t as usize];
    }
    if !make_patches(s) {
        return false;
    }
    make_limiter(s);
    ch[0].noise_index = 0;
    ch[1].noise_index = 0;
    true
}

fn read_header(s: &mut Sbr, b: &mut Bits<'_>) {
    let old = (s.start_freq, s.stop_freq, s.xover, s.freq_scale, s.alter_scale, s.noise_bands);
    let old_limiter = s.limiter_bands;
    s.start = true;
    s.amp_res_header = b.get(1) as u8;
    s.start_freq = b.get(4) as i32;
    s.stop_freq = b.get(4) as i32;
    s.xover = b.get(3) as i32;
    b.get(2);
    let extra1 = b.bit();
    let extra2 = b.bit();
    if extra1 {
        s.freq_scale = b.get(2) as i32;
        s.alter_scale = b.get(1) as i32;
        s.noise_bands = b.get(2) as i32;
    } else {
        s.freq_scale = 2;
        s.alter_scale = 1;
        s.noise_bands = 2;
    }
    if extra2 {
        s.limiter_bands = b.get(2) as i32;
        s.limiter_gains = b.get(2) as i32;
        s.interpol_freq = b.get(1) as i32;
        s.smoothing_mode = b.get(1) as i32;
    } else {
        s.limiter_bands = 2;
        s.limiter_gains = 2;
        s.interpol_freq = 1;
        s.smoothing_mode = 1;
    }
    let now = (s.start_freq, s.stop_freq, s.xover, s.freq_scale, s.alter_scale, s.noise_bands);
    if old != now {
        s.reset = true;
    } else if s.limiter_bands != old_limiter {
        make_limiter(s);
    }
}

fn read_grid(s: &Sbr, b: &mut Bits<'_>, c: &mut Ch) -> bool {
    const CEIL_LOG2: [u32; 6] = [0, 1, 2, 2, 3, 3];
    let old_num_env = i32::from(c.num_env);
    let mut pointer = 0i32;
    let mut trail = 16i32;
    let n: usize;
    c.freq_res[0] = c.freq_res[usize::from(c.num_env)];
    c.amp_res = s.amp_res_header;
    c.t_env_prev_end = c.t_env[usize::from(c.num_env)];
    c.frame_class = b.get(2) as u8;
    match c.frame_class {
        FIXFIX => {
            n = 1 << b.get(2);
            if n > 4 {
                return false;
            }
            if n == 1 {
                c.amp_res = 0;
            }
            c.t_env[0] = 0;
            c.t_env[n] = 16;
            let step = (16 + (n >> 1)) / n;
            for i in 0..n - 1 {
                c.t_env[i + 1] = c.t_env[i].wrapping_add(step as u8);
            }
            let fr = b.get(1) as u8;
            c.freq_res[1..=n].fill(fr);
        }
        FIXVAR => {
            trail += b.get(2) as i32;
            let rel = b.get(2) as usize;
            n = rel + 1;
            c.t_env[0] = 0;
            c.t_env[n] = trail as u8;
            for i in 0..rel {
                c.t_env[n - 1 - i] = c.t_env[n - i].wrapping_sub(2 * b.get(2) as u8).wrapping_sub(2);
            }
            pointer = b.get(CEIL_LOG2[n]) as i32;
            for i in 0..n {
                c.freq_res[n - i] = b.get(1) as u8;
            }
        }
        VARFIX => {
            c.t_env[0] = b.get(2) as u8;
            let rel = b.get(2) as usize;
            n = rel + 1;
            c.t_env[n] = 16;
            for i in 0..rel {
                c.t_env[i + 1] = c.t_env[i].wrapping_add(2 * b.get(2) as u8).wrapping_add(2);
            }
            pointer = b.get(CEIL_LOG2[n]) as i32;
            for i in 1..=n {
                c.freq_res[i] = b.get(1) as u8;
            }
        }
        _ => {
            c.t_env[0] = b.get(2) as u8;
            trail += b.get(2) as i32;
            let lead = b.get(2) as usize;
            let rel = b.get(2) as usize;
            n = lead + rel + 1;
            if n > MAX_ENV {
                return false;
            }
            c.t_env[n] = trail as u8;
            for i in 0..lead {
                c.t_env[i + 1] = c.t_env[i].wrapping_add(2 * b.get(2) as u8).wrapping_add(2);
            }
            for i in 0..rel {
                c.t_env[n - 1 - i] = c.t_env[n - i].wrapping_sub(2 * b.get(2) as u8).wrapping_sub(2);
            }
            pointer = b.get(CEIL_LOG2[n]) as i32;
            for i in 1..=n {
                c.freq_res[i] = b.get(1) as u8;
            }
        }
    }
    c.num_env = n as u8;
    let ni = n as i32;
    if pointer > ni + 1 {
        return false;
    }
    for i in 1..=n {
        if c.t_env[i - 1] as i8 >= c.t_env[i] as i8 {
            return false;
        }
    }
    c.num_noise = if n > 1 { 2 } else { 1 };
    c.t_q[0] = c.t_env[0];
    c.t_q[usize::from(c.num_noise)] = c.t_env[n];
    if c.num_noise > 1 {
        let idx = if c.frame_class == FIXFIX {
            ni >> 1
        } else if c.frame_class & 1 != 0 {
            ni - (pointer - 1).max(1)
        } else if pointer == 0 {
            1
        } else if pointer == 1 {
            ni - 1
        } else {
            pointer - 1
        };
        c.t_q[1] = c.t_env[idx as usize];
    }
    c.l_a[0] = if i32::from(c.l_a[1]) == old_num_env { 0 } else { -1 };
    c.l_a[1] = -1;
    if c.frame_class & 1 != 0 && pointer != 0 {
        c.l_a[1] = (ni + 1 - pointer) as i8;
    } else if c.frame_class == VARFIX && pointer > 1 {
        c.l_a[1] = (pointer - 1) as i8;
    }
    true
}

fn copy_grid(dst: &mut Ch, src: &Ch) {
    dst.freq_res[0] = dst.freq_res[usize::from(dst.num_env)];
    dst.t_env_prev_end = dst.t_env[usize::from(dst.num_env)];
    dst.l_a[0] = if dst.l_a[1] == dst.num_env as i8 { 0 } else { -1 };
    dst.freq_res[1..].copy_from_slice(&src.freq_res[1..]);
    dst.t_env = src.t_env;
    dst.t_q = src.t_q;
    dst.num_env = src.num_env;
    dst.amp_res = src.amp_res;
    dst.num_noise = src.num_noise;
    dst.frame_class = src.frame_class;
    dst.l_a[1] = src.l_a[1];
}

fn read_dtdf(b: &mut Bits<'_>, c: &mut Ch) {
    for i in 0..usize::from(c.num_env) {
        c.df_env[i] = b.get(1) as u8;
    }
    for i in 0..usize::from(c.num_noise) {
        c.df_noise[i] = b.get(1) as u8;
    }
}

fn read_invf(s: &Sbr, b: &mut Bits<'_>, c: &mut Ch) {
    c.invf[1] = c.invf[0];
    for i in 0..s.n_q as usize {
        c.invf[0][i] = b.get(2) as u8;
    }
}

fn read_envelope(s: &Sbr, b: &mut Bits<'_>, c: &mut Ch, ch: usize) -> bool {
    let bal = s.coupling && ch != 0;
    let delta = if bal { 2 } else { 1 };
    let odd = (s.n_high & 1) as usize;
    let res = c.amp_res != 0;
    let (bits, t_huff, f_huff) = match (bal, res) {
        (true, true) => (5, SBR_H_T_ENV_BAL_3_0, SBR_H_F_ENV_BAL_3_0),
        (true, false) => (6, SBR_H_T_ENV_BAL_1_5, SBR_H_F_ENV_BAL_1_5),
        (false, true) => (6, SBR_H_T_ENV_3_0, SBR_H_F_ENV_3_0),
        (false, false) => (7, SBR_H_T_ENV_1_5, SBR_H_F_ENV_1_5),
    };
    let in_range = |v: i16| (0..=127).contains(&v);
    for i in 0..usize::from(c.num_env) {
        let fr = c.freq_res[i + 1];
        let n = (if fr != 0 { s.n_high } else { s.n_low }) as usize;
        if c.df_env[i] != 0 {
            for j in 0..n {
                let k = if fr == c.freq_res[i] {
                    j
                } else if fr != 0 {
                    (j + odd) >> 1
                } else if j != 0 {
                    2 * j - odd
                } else {
                    0
                };
                let v = (i32::from(c.env_q[i][k]) + delta * huff(b, t_huff)) as i16;
                c.env_q[i + 1][j] = v;
                if !in_range(v) {
                    return false;
                }
            }
        } else {
            c.env_q[i + 1][0] = (delta * b.get(bits) as i32) as i16;
            for j in 1..n {
                let v = (i32::from(c.env_q[i + 1][j - 1]) + delta * huff(b, f_huff)) as i16;
                c.env_q[i + 1][j] = v;
                if !in_range(v) {
                    return false;
                }
            }
        }
    }
    c.env_q[0] = c.env_q[usize::from(c.num_env)];
    true
}

fn read_noise(s: &Sbr, b: &mut Bits<'_>, c: &mut Ch, ch: usize) -> bool {
    let bal = s.coupling && ch != 0;
    let delta = if bal { 2 } else { 1 };
    let t_huff = if bal { SBR_H_T_NOISE_BAL_3_0 } else { SBR_H_T_NOISE_3_0 };
    let f_huff = if bal { SBR_H_F_ENV_BAL_3_0 } else { SBR_H_F_ENV_3_0 };
    let in_range = |v: i16| (0..=30).contains(&v);
    let n_q = s.n_q as usize;
    for i in 0..usize::from(c.num_noise) {
        if c.df_noise[i] != 0 {
            for j in 0..n_q {
                let v = (i32::from(c.noise_q[i][j]) + delta * huff(b, t_huff)) as i16;
                c.noise_q[i + 1][j] = v;
                if !in_range(v) {
                    return false;
                }
            }
        } else {
            c.noise_q[i + 1][0] = (delta * b.get(5) as i32) as i16;
            for j in 1..n_q {
                let v = (i32::from(c.noise_q[i + 1][j - 1]) + delta * huff(b, f_huff)) as i16;
                c.noise_q[i + 1][j] = v;
                if !in_range(v) {
                    return false;
                }
            }
        }
    }
    c.noise_q[0] = c.noise_q[usize::from(c.num_noise)];
    true
}

fn read_harmonic(s: &Sbr, b: &mut Bits<'_>, c: &mut Ch) {
    c.add_harmonic_flag = b.bit();
    if c.add_harmonic_flag {
        for i in 0..s.n_high as usize {
            c.add_harmonic[i] = b.get(1) as u8;
        }
    }
}

// Not one select over the two constants: the Xtensa LLVM backend cannot select
// the constant pool it makes of them.
#[inline(never)]
fn exp2_half_odd(e: i32) -> f32 {
    ldexpf(1.41421356237, e >> 1)
}

fn exp2_half(e: i32) -> f32 {
    if e & 1 != 0 { exp2_half_odd(e) } else { ldexpf(1.0, e >> 1) }
}

fn dequant(s: &Sbr, ch: &mut [Ch; 2], channels: usize) {
    const SCALE: f32 = 256.0;
    if channels == 2 && s.coupling {
        let (c0, c1) = ch.split_at_mut(1);
        let (c0, c1) = (&mut c0[0], &mut c1[0]);
        let pan = if c0.amp_res != 0 { 12 } else { 24 };
        for e in 0..usize::from(c0.num_env) {
            let n = if c0.freq_res[e + 1] != 0 { s.n_high } else { s.n_low };
            for k in 0..n as usize {
                let (e0, e1) = (i32::from(c0.env_q[e + 1][k]), i32::from(c1.env_q[e + 1][k]));
                let (mut t1, t2) = if c0.amp_res != 0 {
                    (ldexpf(1.0, e0 + 7), ldexpf(1.0, pan - e1))
                } else {
                    (exp2_half(e0 + 14), exp2_half(pan - e1))
                };
                if t1 > 1e20 {
                    t1 = 0.0;
                }
                let f = t1 / (1.0 + t2);
                c0.env[e][k] = f * SCALE;
                c1.env[e][k] = f * t2 * SCALE;
            }
        }
        for e in 0..usize::from(c0.num_noise) {
            for k in 0..s.n_q as usize {
                let t1 = ldexpf(1.0, 6 - i32::from(c0.noise_q[e + 1][k]) + 1);
                let t2 = ldexpf(1.0, 12 - i32::from(c1.noise_q[e + 1][k]));
                let f = t1 / (1.0 + t2);
                c0.noise[e][k] = f;
                c1.noise[e][k] = f * t2;
            }
        }
        return;
    }
    for c in &mut ch[..channels] {
        for e in 0..usize::from(c.num_env) {
            let n = if c.freq_res[e + 1] != 0 { s.n_high } else { s.n_low };
            for k in 0..n as usize {
                let q = i32::from(c.env_q[e + 1][k]);
                let mut v = if c.amp_res != 0 { ldexpf(1.0, q + 6) } else { exp2_half(q + 12) };
                if v > 1e20 {
                    v = 0.0;
                }
                c.env[e][k] = v * SCALE;
            }
        }
        for e in 0..usize::from(c.num_noise) {
            for k in 0..s.n_q as usize {
                c.noise[e][k] = ldexpf(1.0, 6 - i32::from(c.noise_q[e + 1][k]));
            }
        }
    }
}

unsafe fn qmf_analysis_rows(c: &mut Ch, tables: &Tables, time: *const i32, bands: usize, tmp: *mut i32) {
    unsafe {
        let a = tmp;
        let work = tmp.add(256);
        let u = tmp.add(512);
        core::ptr::copy(c.x_low.add(SLOTS * 64), c.x_low, HF_GEN * 64);
        core::ptr::copy(c.ana_hi.add(FRAME_LEN), c.ana_hi, 288);
        core::ptr::copy(c.ana_lo.add(FRAME_LEN), c.ana_lo, 288);
        for n in 0..FRAME_LEN {
            split(c.ana_hi.add(288 + n), c.ana_lo.add(288 + n), (*time.add(n)).wrapping_mul(1 << 7));
        }
        for g in (0..SLOTS).step_by(4) {
            for l in 0..4 {
                let at = 32 * (g + l) + 256;
                qmf_analysis(u, c.ana_hi.add(at), c.ana_lo.add(at), tables.qmf_c2r);
                *a.add(l) = (*u.add(63)).wrapping_mul(8);
                *a.add(4 + l) = (*u.add(62)).wrapping_mul(8);
                for k in 1..32 {
                    *a.add(8 * k + l) = (*u.add(k - 1)).wrapping_neg().wrapping_mul(8);
                    *a.add(8 * k + 4 + l) = (*u.add(62 - k)).wrapping_mul(8);
                }
            }
            dct4_64x4(a, work, tables.dct64.as_ptr());
            for l in 0..4 {
                let out = c.x_low.add((HF_GEN + g + l) * 64);
                for k in 0..bands {
                    *out.add(2 * k) = *a.add(4 * k + l);
                    *out.add(2 * k + 1) = (*a.add(4 * (63 - k) + l)).wrapping_neg();
                }
            }
        }
    }
}

// Four consecutive slots of 64 complex subbands, row `i` at `x[i]`, into
// `acc[i * 64]`; `tmp` holds 768 words.
unsafe fn qmf_synthesis_rows(c: &mut Ch, tables: &Tables, x: *const *const i32, acc: *mut i32, tmp: *mut i32) {
    unsafe {
        let a = tmp;
        let b = tmp.add(256);
        let work = tmp.add(512);
        syn_in4(a, b, x);
        dct4_64x4(a, work, tables.dct64.as_ptr());
        dct4_64x4(b, work, tables.dct64.as_ptr());
        if c.v_off < 512 {
            core::ptr::copy(c.v_hi.add(c.v_off), c.v_hi.add(V_LEN - 1152), 1152);
            core::ptr::copy(c.v_lo.add(c.v_off), c.v_lo.add(V_LEN - 1152), 1152);
            c.v_off = V_LEN - 1152;
        }
        syn_v4(c.v_hi.add(c.v_off), c.v_lo.add(c.v_off), a, b);
        for l in 0..4 {
            c.v_off -= 128;
            qmf_synthesis(acc.add(64 * l), c.v_hi.add(c.v_off), c.v_lo.add(c.v_off), tables.qmf_c);
        }
    }
}

fn build_row(s: &Sbr, c: &mut Ch, w: &Hf, l: usize, i_temp: usize, row: *mut i32) {
    unsafe {
        core::ptr::write_bytes(row, 0, 128);
        let x_low = c.x_low.add((l + HF_ADJ) * 64);
        if l < i_temp {
            let (kx, m) = (s.kx_prev as usize, s.m_prev as usize);
            core::ptr::copy_nonoverlapping(x_low, row, kx * 2);
            core::ptr::copy_nonoverlapping(c.y_tail.add(l * 128 + 2 * kx), row.add(2 * kx), m * 2);
        } else {
            core::ptr::copy_nonoverlapping(x_low, row, s.kx as usize * 2);
        }
    }
    let (t0, t1) = (2 * usize::from(c.t_env[0]), 2 * usize::from(c.t_env[usize::from(c.num_env)]));
    if s.start && l < SLOTS && l >= t0 && l < t1 {
        sbr_hf::slot(s, c, w, l as i32, if l >= i_temp { row } else { core::ptr::null_mut() });
    }
}

impl State {
    pub(super) fn new(rate: u32, core_channels: usize, channels: usize, ps_on: bool, caps: Caps) -> Result<Box<Self, Caps>> {
        let mut st = Self {
            s: heap::zeroed_box(caps)?,
            ch: heap::zeroed_box(caps)?,
            hf: [None, None],
            ps: None,
            _ana: [None, None],
            _x_low: [None, None],
            _y_tail: [None, None],
            _v: [None, None],
        };
        st.s.rate = rate;
        st.s.kx_prev = 32;
        st.s.noise_table = SBR_NOISE_TABLE;
        turn_off(&mut st.s, &mut st.ch);
        for ch in 0..channels {
            let c = &mut st.ch[ch];
            if ch < core_channels {
                let mut ana: Box<A16<[i16; 2 * ANA_LEN]>, Caps> = heap::zeroed_box(caps)?;
                let mut x_low: Box<[i32; XLOW_SLOTS * 64], Caps> = heap::zeroed_box(caps)?;
                let mut y_tail: Box<[i32; 6 * 128], Caps> = heap::zeroed_box(caps)?;
                c.ana_hi = ana.as_mut_ptr();
                c.ana_lo = unsafe { c.ana_hi.add(ANA_LEN) };
                c.x_low = x_low.as_mut_ptr();
                c.y_tail = y_tail.as_mut_ptr();
                st._ana[ch] = Some(ana);
                st._x_low[ch] = Some(x_low);
                st._y_tail[ch] = Some(y_tail);
                st.hf[ch] = Some(heap::zeroed_box(caps)?);
            }
            let mut v: Box<A16<[i16; 2 * V_LEN]>, Caps> = heap::zeroed_box(caps)?;
            c.v_hi = v.as_mut_ptr();
            c.v_lo = unsafe { c.v_hi.add(V_LEN) };
            c.v_off = V_LEN - 1280;
            c.l_a[1] = -1;
            st._v[ch] = Some(v);
        }
        if ps_on {
            st.ps = Some(heap::zeroed_box(caps)?);
        }
        Box::try_new_in(st, caps).map_err(|_| Error::NoMemory)
    }

    pub(super) fn frame_start(&mut self) {
        self.s.have_data = false;
        self.s.reset = false;
    }

    fn read_data(&mut self, b: &mut Bits<'_>, element: u32, end: u32) -> bool {
        let s = &mut *self.s;
        let (c0, c1) = self.ch.split_at_mut(1);
        let (c0, c1) = (&mut c0[0], &mut c1[0]);
        if element == ID_SCE {
            if b.bit() {
                b.get(4);
            }
            s.coupling = false;
            if !read_grid(s, b, c0) {
                return false;
            }
            read_dtdf(b, c0);
            read_invf(s, b, c0);
            if !read_envelope(s, b, c0, 0) || !read_noise(s, b, c0, 0) {
                return false;
            }
            read_harmonic(s, b, c0);
        } else {
            if b.bit() {
                b.get(8);
            }
            s.coupling = b.bit();
            if s.coupling {
                if !read_grid(s, b, c0) {
                    return false;
                }
                copy_grid(c1, c0);
                read_dtdf(b, c0);
                read_dtdf(b, c1);
                read_invf(s, b, c0);
                c1.invf[1] = c1.invf[0];
                c1.invf[0] = c0.invf[0];
                if !read_envelope(s, b, c0, 0) || !read_noise(s, b, c0, 0) {
                    return false;
                }
                if !read_envelope(s, b, c1, 1) || !read_noise(s, b, c1, 1) {
                    return false;
                }
            } else {
                if !read_grid(s, b, c0) || !read_grid(s, b, c1) {
                    return false;
                }
                read_dtdf(b, c0);
                read_dtdf(b, c1);
                read_invf(s, b, c0);
                read_invf(s, b, c1);
                if !read_envelope(s, b, c0, 0) || !read_envelope(s, b, c1, 1) {
                    return false;
                }
                if !read_noise(s, b, c0, 0) || !read_noise(s, b, c1, 1) {
                    return false;
                }
            }
            read_harmonic(s, b, c0);
            read_harmonic(s, b, c1);
        }
        if b.bit() {
            let mut left = b.get(4) as i32;
            if left == 15 {
                left += b.get(8) as i32;
            }
            left *= 8;
            while left > 7 {
                let id = b.get(2);
                left -= 2;
                match &mut self.ps {
                    Some(ps) if id == 2 && element == ID_SCE => left -= ps.parse(b, left),
                    _ => {
                        b.pos += left as u32;
                        left = 0;
                    }
                }
            }
            if left > 0 {
                b.pos += left as u32;
            }
        }
        !b.overrun() && b.pos <= end
    }

    pub(super) fn parse(&mut self, b: &mut Bits<'_>, element: u32, core_channels: usize, bits: u32, crc: bool) -> Result<()> {
        let end = b.pos.wrapping_add(bits);
        if (element == ID_SCE) != (core_channels == 1) {
            return Err(Error::InvalidData);
        }
        if crc {
            b.get(10);
        }
        if b.bit() {
            read_header(&mut self.s, b);
        }
        if self.s.reset && !make_tables(&mut self.s, &mut self.ch) {
            turn_off(&mut self.s, &mut self.ch);
            return Err(Error::InvalidData);
        }
        if !self.s.start {
            return Ok(());
        }
        if !self.read_data(b, element, end) {
            turn_off(&mut self.s, &mut self.ch);
            return Err(Error::InvalidData);
        }
        self.s.have_data = true;
        Ok(())
    }

    pub(super) fn reset(&mut self) {
        for c in self.ch.iter_mut() {
            let (ana_hi, ana_lo, v_hi, v_lo) = (c.ana_hi, c.ana_lo, c.v_hi, c.v_lo);
            let (x_low, y_tail) = (c.x_low, c.y_tail);
            unsafe {
                core::ptr::write_bytes(core::ptr::from_mut(c), 0, 1);
                if !ana_hi.is_null() {
                    core::ptr::write_bytes(ana_hi, 0, 2 * ANA_LEN);
                }
                if !x_low.is_null() {
                    core::ptr::write_bytes(x_low, 0, XLOW_SLOTS * 64);
                }
                if !y_tail.is_null() {
                    core::ptr::write_bytes(y_tail, 0, 6 * 128);
                }
                if !v_hi.is_null() {
                    core::ptr::write_bytes(v_hi, 0, 2 * V_LEN);
                }
            }
            c.ana_hi = ana_hi;
            c.ana_lo = ana_lo;
            c.v_hi = v_hi;
            c.v_lo = v_lo;
            c.x_low = x_low;
            c.y_tail = y_tail;
            c.l_a[1] = -1;
            c.v_off = V_LEN - 1280;
        }
        self.s.have_data = false;
        self.s.m_prev = 0;
        self.s.kx_prev = self.s.kx;
        if let Some(ps) = &mut self.ps {
            unsafe { core::ptr::write_bytes(core::ptr::from_mut(&mut **ps), 0, 1) };
        }
    }

    fn finish_channel(&mut self, ch: usize) {
        let s = &*self.s;
        if !s.start {
            return;
        }
        let c = &mut self.ch[ch];
        let w = self.hf[ch].as_deref().expect("core channel");
        let (t0, t1) = (2 * usize::from(c.t_env[0]), 2 * usize::from(c.t_env[usize::from(c.num_env)]));
        for l in SLOTS..t1 {
            if l >= t0 {
                sbr_hf::slot(s, c, w, l as i32, unsafe { c.y_tail.add((l - SLOTS) * 128) });
            }
        }
        sbr_hf::finish(s, c, w);
    }
}

impl Dec {
    fn start_channel(&mut self, st: &mut State, ch: usize) -> usize {
        let scratch = self.scratch.as_mut_ptr();
        let time = unsafe { scratch.add(FRAME_LEN) };
        let tables = self.tables;
        unsafe { filterbank(&mut self.ch[ch], &tables, scratch, time) };
        self.prof_stage(ProfileStage::Filterbank);
        let s = &*st.s;
        let c = &mut st.ch[ch];
        unsafe { qmf_analysis_rows(c, &tables, time, s.kx.max(s.kx_prev) as usize, scratch) };
        let kx_prev = s.kx_prev as usize;
        for l in 0..HF_GEN {
            unsafe { core::ptr::write_bytes(c.x_low.add(l * 64 + 2 * kx_prev), 0, (32 - kx_prev) * 2) };
        }
        self.prof_stage(ProfileStage::QmfAnalysis);
        if s.start {
            sbr_hf::prepare(s, c, st.hf[ch].as_deref_mut().expect("core channel"));
        }
        self.prof_stage(ProfileStage::Sbr);
        (2 * i32::from(c.t_env_prev_end) - SLOTS as i32).max(0) as usize
    }

    pub(super) fn sbr_apply(&mut self, out: &mut [i16]) {
        let mut st = self.sbr.take().expect("sbr on");
        let out = out.as_mut_ptr();
        if st.s.start && !st.s.have_data {
            turn_off(&mut st.s, &mut st.ch);
        }
        let channels = self.core_channels;
        if st.s.start {
            dequant(&st.s, &mut st.ch, channels);
        }
        let tables = self.tables;
        let tmp = self.scratch.as_mut_ptr();
        let (acc0, acc1, rows) = unsafe { (tmp.add(768), tmp.add(1024), tmp.add(1280)) };
        let rows_of = |r: *mut i32| unsafe { [r.cast_const(), r.add(128).cast_const(), r.add(256).cast_const(), r.add(384).cast_const()] };
        if self.ps_on {
            let i_temp = self.start_channel(&mut st, 0);
            let ps_ready = st.s.start && st.ps.as_ref().is_some_and(|p| p.ready());
            let (l_rows, r_rows) = (rows, unsafe { rows.add(512) });
            if ps_ready {
                let x_low = unsafe { st.ch[0].x_low.add(HF_ADJ * 64) };
                let top = (st.s.kx + st.s.m) as usize;
                st.ps.as_mut().expect("ps on").begin(x_low, top);
            }
            self.prof_stage(ProfileStage::Ps);
            let r0 = rows_of(l_rows);
            let r1 = rows_of(r_rows);
            for l in (0..SLOTS).step_by(4) {
                let w = st.hf[0].as_deref().expect("core channel");
                for i in 0..4 {
                    build_row(&st.s, &mut st.ch[0], w, l + i, i_temp, unsafe { l_rows.add(i * 128) });
                }
                self.prof_stage(ProfileStage::Sbr);
                if ps_ready {
                    st.ps.as_mut().expect("ps on").slots(l_rows, r_rows, l, 4);
                } else {
                    unsafe { core::ptr::copy_nonoverlapping(l_rows, r_rows, 4 * 128) };
                }
                self.prof_stage(ProfileStage::Ps);
                unsafe {
                    qmf_synthesis_rows(&mut st.ch[0], &tables, r0.as_ptr(), acc0, tmp);
                    qmf_synthesis_rows(&mut st.ch[1], &tables, r1.as_ptr(), acc1, tmp);
                    for i in 0..4 {
                        pack(out.add((l + i) * 128), acc0.add(64 * i), acc1.add(64 * i), 64, 7);
                    }
                }
                self.prof_stage(ProfileStage::QmfSynthesis);
            }
            if ps_ready {
                st.ps.as_mut().expect("ps on").end();
            }
            st.finish_channel(0);
            self.prof_stage(ProfileStage::Sbr);
        } else {
            let mut i_temp = [0usize; 2];
            for ch in 0..channels {
                i_temp[ch] = self.start_channel(&mut st, ch);
            }
            let r = rows_of(rows);
            for l in (0..SLOTS).step_by(4) {
                for ch in 0..channels {
                    let w = st.hf[ch].as_deref().expect("core channel");
                    for i in 0..4 {
                        build_row(&st.s, &mut st.ch[ch], w, l + i, i_temp[ch], unsafe { rows.add(i * 128) });
                    }
                    self.prof_stage(ProfileStage::Sbr);
                    let acc = if ch == 0 { acc0 } else { acc1 };
                    unsafe { qmf_synthesis_rows(&mut st.ch[ch], &tables, r.as_ptr(), acc, tmp) };
                    self.prof_stage(ProfileStage::QmfSynthesis);
                }
                for i in 0..4 {
                    unsafe {
                        let right = if channels == 2 { acc1.add(64 * i).cast_const() } else { core::ptr::null() };
                        pack(out.add((l + i) * 64 * channels), acc0.add(64 * i), right, 64, 7);
                    }
                }
            }
            for ch in 0..channels {
                st.finish_channel(ch);
            }
            self.prof_stage(ProfileStage::Sbr);
        }
        st.s.kx_prev = st.s.kx;
        st.s.m_prev = st.s.m;
        self.sbr = Some(st);
    }
}
