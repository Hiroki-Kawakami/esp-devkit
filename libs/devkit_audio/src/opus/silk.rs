/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::Zeroable;
use devkit::math::sqrtf;

use super::rc::{Rc, ilog};
use super::silk_tables::*;

const MAX_FRAME: usize = 320;
const MAX_SUBFR: usize = 80;
const LTP_MEM: usize = 320;
const MAX_ORDER: usize = 16;

const TYPE_INACTIVE: i32 = 0;
const TYPE_UNVOICED: i32 = 1;
const TYPE_VOICED: i32 = 2;

const CODE_INDEPENDENTLY: i32 = 0;
const CODE_INDEPENDENTLY_NO_LTP_SCALING: i32 = 1;
const CODE_CONDITIONALLY: i32 = 2;

#[derive(Clone, Copy)]
struct Resampler {
    iir: [i32; 6],
    fir: [i16; 8],
    delay: [i16; 16],
    input_delay: usize,
    fs_in_khz: i32,
    inv_ratio_q16: i32,
}

#[derive(Clone, Copy)]
struct Indices {
    signal_type: i32,
    offset_type: i32,
    gains: [i32; 4],
    nlsf: [i32; MAX_ORDER + 1],
    interp: i32,
    lag_index: i32,
    contour: i32,
    per_index: i32,
    ltp_index: [i32; 4],
    ltp_scale: i32,
    seed: i32,
}

#[derive(Clone, Copy)]
struct FrameCtrl {
    gain_q16: [i32; 4],
    a_q12: [[i16; MAX_ORDER]; 2],
    pitch: [i32; 4],
    ltp_q7: [[i8; 5]; 4],
    ltp_scale_q14: i32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Channel {
    fs_khz: i32,
    nb_subfr: i32,
    subfr_len: i32,
    frame_len: i32,
    order: i32,
    frames_per_packet: i32,
    frames_decoded: i32,
    vad: [i32; 3],
    lbrr_flag: i32,
    lbrr: [i32; 3],
    first_frame_after_reset: bool,
    last_gain_index: i32,
    lag_prev: i32,
    prev_signal_type: i32,
    ec_prev_signal_type: i32,
    ec_prev_lag_index: i32,
    prev_nlsf: [i16; MAX_ORDER],
    ind: Indices,
    out: [i16; LTP_MEM + 2 * MAX_SUBFR],
    lpc_q14: [i32; MAX_ORDER],
    prev_gain_q16: i32,
    last: FrameCtrl,
    last_exc_q14: i32,
    loss_count: i32,
    plc_seed: u32,
    rs: Resampler,
}

#[repr(C)]
struct Work {
    resampled: [i16; MAX_FRAME * 6],
    upsampled: [i16; 2 * 160 + 8],
    res: [i32; MAX_SUBFR],
    pulses: [i32; MAX_FRAME + 16],
    exc_q14: [i32; MAX_FRAME],
    ltp: [i16; LTP_MEM],
    ltp_q15: [i32; LTP_MEM + MAX_FRAME],
    lpc_q14: [i32; MAX_ORDER + MAX_SUBFR],
}

#[repr(C)]
pub(super) struct Silk {
    channels: i32,
    channels_internal: i32,
    prev_decode_only_middle: i32,
    pred_prev_q13: [i32; 2],
    s_mid: [i16; 2],
    s_side: [i16; 2],
    ch: [Channel; 2],
    buf: [[i16; MAX_FRAME + 2]; 2],
    w: Work,
}

unsafe impl Zeroable for Silk {}

fn resampler_init(r: &mut Resampler, fs_in_khz: i32) {
    *r = unsafe { core::mem::zeroed() };
    r.fs_in_khz = fs_in_khz;
    r.input_delay = match fs_in_khz {
        8 => 0,
        12 => 4,
        _ => 7,
    };
    let fs_in = fs_in_khz * 1000;
    let mut inv = ((fs_in << 15) / 48000) << 2;
    while (((i64::from(inv) * 48000) >> 16) as i32) < fs_in << 1 {
        inv += 1;
    }
    r.inv_ratio_q16 = inv;
}

#[inline(always)]
fn mulwb(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> 16) as i32
}

#[inline(always)]
fn sat16(v: i32) -> i16 {
    v.clamp(-32768, 32767) as i16
}

unsafe fn up2(s: &mut [i32; 6], out: *mut i16, input: *const i16, len: usize) {
    let states = unsafe { &mut *s.as_mut_ptr().cast::<[[i32; 3]; 2]>() };
    for k in 0..len {
        let x = i32::from(unsafe { *input.add(k) }) << 10;
        for branch in 0..2 {
            let c = &SILK_UP2_ALLPASS[branch];
            let [s0, s1, s2] = &mut states[branch];
            let mut y = x.wrapping_sub(*s0);
            let mut t = mulwb(y, i32::from(c[0]));
            let o1 = s0.wrapping_add(t);
            *s0 = x.wrapping_add(t);
            y = o1.wrapping_sub(*s1);
            t = mulwb(y, i32::from(c[1]));
            let o2 = s1.wrapping_add(t);
            *s1 = o1.wrapping_add(t);
            y = o2.wrapping_sub(*s2);
            t = y.wrapping_add(mulwb(y, i32::from(c[2])));
            let o3 = s2.wrapping_add(t);
            *s2 = o2.wrapping_add(t);
            unsafe { *out.add(2 * k + branch) = sat16(o3.wrapping_add(512) >> 10) };
        }
    }
}

unsafe fn interpolate(out: *mut i16, buf: *const i16, max_index: i32, step: i32) -> *mut i16 {
    let mut out = out;
    let mut index = 0;
    while index < max_index {
        let phase = (((index & 0xFFFF) * 12) >> 16) as usize;
        unsafe {
            let p = buf.add((index >> 16) as usize);
            let lo = SILK_FRAC_FIR.get_unchecked(phase);
            let hi = SILK_FRAC_FIR.get_unchecked(11 - phase);
            let m = |i: usize, c: i16| i32::from(*p.add(i)) * i32::from(c);
            let v = m(0, lo[0]) + m(1, lo[1]) + m(2, lo[2]) + m(3, lo[3]) + m(4, hi[3]) + m(5, hi[2]) + m(6, hi[1]) + m(7, hi[0]);
            *out = sat16((v + 16384) >> 15);
            out = out.add(1);
        }
        index += step;
    }
    out
}

unsafe fn resample_block(r: &mut Resampler, buf: *mut i16, out: *mut i16, input: *const i16, len: i32) -> *mut i16 {
    let (mut out, mut input, mut len) = (out, input, len);
    unsafe {
        core::ptr::copy_nonoverlapping(r.fir.as_ptr(), buf, 8);
        let batch = r.fs_in_khz * 10;
        loop {
            let n = len.min(batch);
            up2(&mut r.iir, buf.add(8), input, n as usize);
            out = interpolate(out, buf, n << 17, r.inv_ratio_q16);
            input = input.add(n as usize);
            len -= n;
            if len <= 0 {
                core::ptr::copy_nonoverlapping(buf.add(2 * n as usize), r.fir.as_mut_ptr(), 8);
                return out;
            }
            core::ptr::copy(buf.add(2 * n as usize), buf, 8);
        }
    }
}

// `buf` holds 2 * 160 + 8 samples.
unsafe fn resample(r: &mut Resampler, buf: *mut i16, out: *mut i16, input: *const i16, len: i32) {
    let ms = r.fs_in_khz;
    let delay = r.input_delay;
    let fresh = ms as usize - delay;
    unsafe {
        core::ptr::copy_nonoverlapping(input, r.delay.as_mut_ptr().add(delay), fresh);
        let d = r.delay.as_ptr();
        let out = resample_block(r, buf, out, d, ms);
        resample_block(r, buf, out, input.add(fresh), len - ms);
        core::ptr::copy_nonoverlapping(input.add(len as usize - delay), r.delay.as_mut_ptr(), delay);
    }
}

fn channel_init(c: &mut Channel) {
    *c = unsafe { core::mem::zeroed() };
    c.first_frame_after_reset = true;
    c.last_gain_index = 10;
    c.lag_prev = 100;
    c.prev_gain_q16 = 65536;
}

fn channel_set_fs(c: &mut Channel, fs_khz: i32) {
    c.subfr_len = 5 * fs_khz;
    let frame_len = c.nb_subfr * c.subfr_len;
    if c.fs_khz != fs_khz {
        resampler_init(&mut c.rs, fs_khz);
    }
    if c.fs_khz != fs_khz || frame_len != c.frame_len {
        if c.fs_khz != fs_khz {
            c.order = if fs_khz == 16 { 16 } else { 10 };
            c.first_frame_after_reset = true;
            c.lag_prev = 100;
            c.last_gain_index = 10;
            c.prev_signal_type = TYPE_INACTIVE;
            c.out = [0; LTP_MEM + 2 * MAX_SUBFR];
            c.lpc_q14 = [0; MAX_ORDER];
        }
        c.fs_khz = fs_khz;
        c.frame_len = frame_len;
    }
}

fn decode_indices(c: &mut Channel, rc: &mut Rc<'_>, frame: usize, lbrr: bool, cond: i32) {
    let wb = c.fs_khz == 16;
    let kind = if lbrr || c.vad[frame] != 0 { rc.icdf(&SILK_TYPE_ACTIVE_ICDF, 8) + 2 } else { rc.icdf(&SILK_TYPE_INACTIVE_ICDF, 8) };
    let ind = &mut c.ind;
    ind.signal_type = kind >> 1;
    ind.offset_type = kind & 1;

    if cond == CODE_CONDITIONALLY {
        ind.gains[0] = rc.icdf(&SILK_DELTA_GAIN_ICDF, 8);
    } else {
        ind.gains[0] = rc.icdf(&SILK_GAIN_MSB_ICDF[ind.signal_type as usize], 8) << 3;
        ind.gains[0] += rc.icdf(&SILK_UNIFORM8_ICDF, 8);
    }
    for i in 1..c.nb_subfr as usize {
        ind.gains[i] = rc.icdf(&SILK_DELTA_GAIN_ICDF, 8);
    }

    let i1 = rc.icdf(&SILK_LSF_S1_ICDF[((ind.signal_type >> 1) + 2 * i32::from(wb)) as usize], 8);
    ind.nlsf[0] = i1;
    let i1 = i1 as usize;
    for k in 0..c.order as usize {
        let book = if wb { 8 + usize::from(SILK_LSF_SEL_WB[i1][k]) } else { usize::from(SILK_LSF_SEL_NBMB[i1][k]) };
        let mut ix = rc.icdf(&SILK_LSF_S2_ICDF[book], 8);
        if ix == 0 {
            ix -= rc.icdf(&SILK_LSF_EXT_ICDF, 8);
        } else if ix == 8 {
            ix += rc.icdf(&SILK_LSF_EXT_ICDF, 8);
        }
        ind.nlsf[k + 1] = ix - 4;
    }
    ind.interp = if c.nb_subfr == 4 { rc.icdf(&SILK_LSF_INTERP_ICDF, 8) } else { 4 };

    if ind.signal_type == TYPE_VOICED {
        let mut absolute = true;
        if cond == CODE_CONDITIONALLY && c.ec_prev_signal_type == TYPE_VOICED {
            let delta = rc.icdf(&SILK_PITCH_DELTA_ICDF, 8);
            if delta > 0 {
                ind.lag_index = c.ec_prev_lag_index + delta - 9;
                absolute = false;
            }
        }
        if absolute {
            let low: &[u8] = match c.fs_khz {
                8 => &SILK_PITCH_LOW_NB_ICDF,
                12 => &SILK_PITCH_LOW_MB_ICDF,
                _ => &SILK_PITCH_LOW_WB_ICDF,
            };
            ind.lag_index = rc.icdf(&SILK_PITCH_HIGH_ICDF, 8) * (c.fs_khz >> 1);
            ind.lag_index += rc.icdf(low, 8);
        }
        c.ec_prev_lag_index = ind.lag_index;
        let contour: &[u8] = match (c.fs_khz == 8, c.nb_subfr == 4) {
            (true, true) => &SILK_CONTOUR_NB20_ICDF,
            (true, false) => &SILK_CONTOUR_NB10_ICDF,
            (false, true) => &SILK_CONTOUR_WB20_ICDF,
            (false, false) => &SILK_CONTOUR_WB10_ICDF,
        };
        ind.contour = rc.icdf(contour, 8);
        ind.per_index = rc.icdf(&SILK_LTP_PER_ICDF, 8);
        let filter: &[u8] = match ind.per_index {
            0 => &SILK_LTP_FILTER0_ICDF,
            1 => &SILK_LTP_FILTER1_ICDF,
            _ => &SILK_LTP_FILTER2_ICDF,
        };
        for k in 0..c.nb_subfr as usize {
            ind.ltp_index[k] = rc.icdf(filter, 8);
        }
        ind.ltp_scale = if cond == CODE_INDEPENDENTLY { rc.icdf(&SILK_LTP_SCALE_ICDF, 8) } else { 0 };
    }
    c.ec_prev_signal_type = ind.signal_type;
    ind.seed = rc.icdf(&SILK_UNIFORM4_ICDF, 8);
}

fn shell_split(rc: &mut Rc<'_>, p: i32, table: usize) -> (i32, i32) {
    if p > 0 {
        let a = rc.icdf(&SILK_SHELL_ICDF[table][p as usize - 1], 8);
        (a, p - a)
    } else {
        (0, 0)
    }
}

fn shell_decode(rc: &mut Rc<'_>, out: &mut [i32], p16: i32) {
    let mut p8 = [0; 2];
    let mut p4 = [0; 4];
    let mut p2 = [0; 8];
    (p8[0], p8[1]) = shell_split(rc, p16, 0);
    for i in 0..2 {
        (p4[2 * i], p4[2 * i + 1]) = shell_split(rc, p8[i], 1);
        for j in 2 * i..2 * i + 2 {
            (p2[2 * j], p2[2 * j + 1]) = shell_split(rc, p4[j], 2);
            for k in 2 * j..2 * j + 2 {
                (out[2 * k], out[2 * k + 1]) = shell_split(rc, p2[k], 3);
            }
        }
    }
}

fn decode_pulses(rc: &mut Rc<'_>, pulses: &mut [i32; MAX_FRAME + 16], signal_type: i32, offset_type: i32, frame_len: i32) {
    let mut sums = [0i32; 20];
    let mut lsbs = [0i32; 20];
    let rate = rc.icdf(&SILK_RATE_LEVEL_ICDF[(signal_type >> 1) as usize], 8) as usize;
    let blocks = ((frame_len + 15) >> 4) as usize;
    for i in 0..blocks {
        lsbs[i] = 0;
        sums[i] = rc.icdf(&SILK_PULSE_COUNT_ICDF[rate], 8);
        while sums[i] == 17 {
            lsbs[i] += 1;
            sums[i] = rc.icdf(&SILK_PULSE_COUNT_ICDF[if lsbs[i] == 10 { 10 } else { 9 }], 8);
        }
    }
    for i in 0..blocks {
        let block = &mut pulses[16 * i..16 * i + 16];
        if sums[i] > 0 {
            shell_decode(rc, block, sums[i]);
        } else {
            block.fill(0);
        }
    }
    for i in 0..blocks {
        if lsbs[i] == 0 {
            continue;
        }
        for v in &mut pulses[16 * i..16 * i + 16] {
            for _ in 0..lsbs[i] {
                *v = (*v << 1) + rc.icdf(&SILK_LSB_ICDF, 8);
            }
        }
    }
    let sign = &SILK_SIGN_ICDF[7 * (2 * signal_type + offset_type) as usize..];
    for i in 0..blocks {
        if sums[i] == 0 && lsbs[i] == 0 {
            continue;
        }
        let icdf = [sign[sums[i].min(6) as usize], 0];
        for q in &mut pulses[16 * i..16 * i + 16] {
            if *q > 0 && rc.icdf(&icdf, 8) == 0 {
                *q = -*q;
            }
        }
    }
}

fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    }
    if in_log_q7 >= 3967 {
        return i32::MAX;
    }
    let i = in_log_q7 >> 7;
    let f = in_log_q7 & 127;
    let out = 1i32 << i;
    let frac = (-174 * f * (128 - f) >> 16) + f;
    if i < 16 { out + ((out * frac) >> 7) } else { out + (out >> 7) * frac }
}

fn dequant_gains(c: &mut Channel, gain_q16: &mut [i32; 4], conditional: bool) {
    let mut prev = c.last_gain_index;
    for k in 0..c.nb_subfr as usize {
        let g = c.ind.gains[k];
        if k == 0 && !conditional {
            prev = g.max(prev - 16);
        } else {
            prev = (2 * g - 16).max(prev + g - 4);
        }
        prev = prev.clamp(0, 63);
        gain_q16[k] = log2lin(((0x1D1C71 * prev) >> 16) + 2090);
    }
    c.last_gain_index = prev;
}

fn decode_nlsf(c: &Channel, nlsf: &mut [i16; MAX_ORDER]) {
    let wb = c.fs_khz == 16;
    let d = c.order as usize;
    let i1 = c.ind.nlsf[0] as usize;
    let cb: &[u8] = if wb { &SILK_LSF_CB_WB[i1] } else { &SILK_LSF_CB_NBMB[i1] };
    let qstep = if wb { 9830 } else { 11796 };
    let mut res_q10 = [0i32; MAX_ORDER];
    for k in (0..d).rev() {
        let mut pred = 0;
        if k + 1 < d {
            let w = if wb {
                SILK_LSF_PRED_WB[usize::from(SILK_LSF_PRED_SEL_WB[i1][k])][k]
            } else {
                SILK_LSF_PRED_NBMB[usize::from(SILK_LSF_PRED_SEL_NBMB[i1][k])][k]
            };
            pred = (res_q10[k + 1] * i32::from(w)) >> 8;
        }
        let i2 = c.ind.nlsf[k + 1];
        let v = (i2 << 10) - 102 * i2.signum();
        res_q10[k] = pred + ((v * qstep) >> 16);
    }
    for k in 0..d {
        let cur = i32::from(cb[k]);
        let prev = if k > 0 { i32::from(cb[k - 1]) } else { 0 };
        let next = if k + 1 < d { i32::from(cb[k + 1]) } else { 256 };
        let w2 = (1024 / (cur - prev) + 1024 / (next - cur)) << 16;
        let i = ilog(w2 as u32);
        let f = (w2 >> (i - 8)) & 127;
        let y = (if i & 1 != 0 { 32768 } else { 46214 }) >> ((32 - i) >> 1);
        let w_q9 = y + ((213 * f * y) >> 16);
        let v = (cur << 7) + (res_q10[k] << 14) / w_q9;
        nlsf[k] = v.clamp(0, 32767) as i16;
    }
}

fn stabilize_nlsf(nlsf: &mut [i16; MAX_ORDER], min_delta: &[i16], d: usize) {
    let md = |i: usize| i32::from(min_delta[i]);
    for _ in 0..20 {
        let mut lowest = i32::MAX;
        let mut at = 0;
        for i in 0..=d {
            let prev = if i > 0 { i32::from(nlsf[i - 1]) } else { 0 };
            let cur = if i < d { i32::from(nlsf[i]) } else { 32768 };
            let diff = cur - prev - md(i);
            if diff < lowest {
                lowest = diff;
                at = i;
            }
        }
        if lowest >= 0 {
            return;
        }
        if at == 0 {
            nlsf[0] = min_delta[0];
        } else if at == d {
            nlsf[d - 1] = (32768 - md(d)) as i16;
        } else {
            let mut min_center = md(at) >> 1;
            for k in 0..at {
                min_center += md(k);
            }
            let mut max_center = 32768 - (md(at) >> 1);
            for k in (at + 1..=d).rev() {
                max_center -= md(k);
            }
            let center = ((i32::from(nlsf[at - 1]) + i32::from(nlsf[at]) + 1) >> 1).max(min_center).min(max_center);
            nlsf[at - 1] = (center - (md(at) >> 1)) as i16;
            nlsf[at] = (i32::from(nlsf[at - 1]) + md(at)) as i16;
        }
    }
    for i in 1..d {
        let v = nlsf[i];
        let mut j = i;
        while j > 0 && nlsf[j - 1] > v {
            nlsf[j] = nlsf[j - 1];
            j -= 1;
        }
        nlsf[j] = v;
    }
    for k in 0..d {
        let floor_v = ((if k > 0 { i32::from(nlsf[k - 1]) } else { 0 }) + md(k)).min(32767);
        if i32::from(nlsf[k]) < floor_v {
            nlsf[k] = floor_v as i16;
        }
    }
    for k in (0..d).rev() {
        let ceil_v = (if k + 1 < d { i32::from(nlsf[k + 1]) } else { 32768 }) - md(k + 1);
        if i32::from(nlsf[k]) > ceil_v {
            nlsf[k] = ceil_v as i16;
        }
    }
}

fn bwexpand32(a: &mut [i32; MAX_ORDER], d: usize, chirp_q16: i32) {
    let c0 = i64::from(chirp_q16);
    let mut chirp_q16 = chirp_q16;
    for v in &mut a[..d - 1] {
        *v = ((i64::from(chirp_q16) * i64::from(*v)) >> 16) as i32;
        chirp_q16 = ((c0 * i64::from(chirp_q16) + 32768) >> 16) as i32;
    }
    a[d - 1] = ((i64::from(chirp_q16) * i64::from(a[d - 1])) >> 16) as i32;
}

// RFC 6716 4.2.7.5.8, with the RFC 8251 overflow check: false if unstable.
fn lpc_stable(a_q12: &[i16; MAX_ORDER], d: usize) -> bool {
    let mut a = [[0i32; MAX_ORDER]; 2];
    let mut dc = 0;
    let mut cur_i = d & 1;
    for k in 0..d {
        dc += i32::from(a_q12[k]);
        a[cur_i][k] = i32::from(a_q12[k]) << 12;
    }
    if dc >= 4096 {
        return false;
    }
    let mut inv_gain = 1i64 << 30;
    for k in (0..d).rev() {
        let cur = a[cur_i];
        if cur[k] > 16773022 || cur[k] < -16773022 {
            return false;
        }
        let rc = -(cur[k] * 128);
        let div = ((1i64 << 30) - ((i64::from(rc) * i64::from(rc)) >> 32)) as i32;
        inv_gain = ((inv_gain * i64::from(div)) >> 32) << 2;
        if inv_gain < 107374 {
            return false;
        }
        if k == 0 {
            break;
        }
        let b1 = ilog(div as u32);
        let b2 = b1 - 16;
        let inv = ((1 << 29) - 1) / (div >> (b2 + 1));
        let err = (1 << 29) - ((i64::from(div << (15 - b2)) * i64::from(inv)) >> 16) as i32;
        let gain = (inv << 16).wrapping_add(((i64::from(err) * i64::from(inv)) >> 13) as i32);
        let next_i = k & 1;
        for n in 0..k {
            let t = i64::from(cur[n]) - ((i64::from(cur[k - n - 1]) * i64::from(rc) + (1 << 30)) >> 31);
            let num = t.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            let v = (i64::from(num) * i64::from(gain) + (1i64 << (b1 - 1))) >> b1;
            if v > i64::from(i32::MAX) || v < i64::from(i32::MIN) {
                return false;
            }
            a[next_i][n] = v as i32;
        }
        cur_i = next_i;
    }
    true
}

fn nlsf_to_lpc(nlsf: &[i16; MAX_ORDER], d: usize, a_q12: &mut [i16; MAX_ORDER]) {
    let order: &[u8] = if d == 16 { &SILK_LSF_ORDER_WB } else { &SILK_LSF_ORDER_NBMB };
    let mut c = [0i32; MAX_ORDER];
    for k in 0..d {
        let i = (nlsf[k] >> 8) as usize;
        let f = i32::from(nlsf[k] & 255);
        let (lo, hi) = (i32::from(SILK_LSF_COS_Q12[i]), i32::from(SILK_LSF_COS_Q12[i + 1]));
        c[usize::from(order[k])] = (lo * 256 + (hi - lo) * f + 4) >> 3;
    }
    let d2 = d / 2;
    let mut polys = [[0i32; MAX_ORDER / 2 + 2]; 2];
    for pass in 0..2 {
        let poly = &mut polys[pass];
        let cs = &c[pass..];
        poly[0] = 1 << 16;
        poly[1] = -cs[0];
        for k in 1..d2 {
            let ck = i64::from(cs[2 * k]);
            poly[k + 1] = (2 * poly[k - 1]).wrapping_sub(((ck * i64::from(poly[k]) + 32768) >> 16) as i32);
            for j in (2..=k).rev() {
                poly[j] = poly[j].wrapping_add(poly[j - 2].wrapping_sub(((ck * i64::from(poly[j - 1]) + 32768) >> 16) as i32));
            }
            poly[1] -= ck as i32;
        }
    }
    let [p, q] = polys;
    let mut a32 = [0i32; MAX_ORDER];
    for k in 0..d2 {
        let pt = p[k + 1] + p[k];
        let qt = q[k + 1] - q[k];
        a32[k] = -qt - pt;
        a32[d - k - 1] = qt - pt;
    }
    let mut round = 0;
    while round < 10 {
        let mut maxabs = 0;
        let mut at = 0;
        for (k, v) in a32[..d].iter().enumerate() {
            if v.abs() > maxabs {
                maxabs = v.abs();
                at = k;
            }
        }
        maxabs = (maxabs + 16) >> 5;
        if maxabs <= 32767 {
            break;
        }
        maxabs = maxabs.min(163838);
        let sc = 65470 - ((maxabs - 32767) << 14) / ((maxabs * (at as i32 + 1)) >> 2);
        bwexpand32(&mut a32, d, sc);
        round += 1;
    }
    if round == 10 {
        for k in 0..d {
            a_q12[k] = sat16((a32[k] + 16) >> 5);
            a32[k] = i32::from(a_q12[k]) << 5;
        }
    } else {
        for k in 0..d {
            a_q12[k] = ((a32[k] + 16) >> 5) as i16;
        }
    }
    for i in 0..16 {
        if lpc_stable(a_q12, d) {
            break;
        }
        bwexpand32(&mut a32, d, 65536 - (2 << i));
        for k in 0..d {
            a_q12[k] = ((a32[k] + 16) >> 5) as i16;
        }
    }
}

fn decode_parameters(c: &mut Channel, ctl: &mut FrameCtrl, cond: i32) {
    dequant_gains(c, &mut ctl.gain_q16, cond == CODE_CONDITIONALLY);
    let d = c.order as usize;
    let mut nlsf = [0i16; MAX_ORDER];
    decode_nlsf(c, &mut nlsf);
    let spacing: &[i16] = if d == 16 { &SILK_LSF_MIN_SPACING_WB } else { &SILK_LSF_MIN_SPACING_NBMB };
    stabilize_nlsf(&mut nlsf, spacing, d);
    nlsf_to_lpc(&nlsf, d, &mut ctl.a_q12[1]);
    if c.first_frame_after_reset {
        c.ind.interp = 4;
    }
    if c.ind.interp < 4 {
        let mut mid = [0i16; MAX_ORDER];
        for i in 0..d {
            let prev = i32::from(c.prev_nlsf[i]);
            mid[i] = (prev + ((c.ind.interp * (i32::from(nlsf[i]) - prev)) >> 2)) as i16;
        }
        nlsf_to_lpc(&mid, d, &mut ctl.a_q12[0]);
    } else {
        ctl.a_q12[0] = ctl.a_q12[1];
    }
    c.prev_nlsf[..d].copy_from_slice(&nlsf[..d]);

    if c.ind.signal_type == TYPE_VOICED {
        let min_lag = 2 * c.fs_khz;
        let max_lag = 18 * c.fs_khz;
        let lag = min_lag + c.ind.lag_index;
        let contour = c.ind.contour as usize;
        for k in 0..c.nb_subfr as usize {
            let off = match (c.fs_khz == 8, c.nb_subfr == 4) {
                (true, true) => SILK_CONTOUR_NB20[contour][k],
                (true, false) => SILK_CONTOUR_NB10[contour][k],
                (false, true) => SILK_CONTOUR_WB20[contour][k],
                (false, false) => SILK_CONTOUR_WB10[contour][k],
            };
            ctl.pitch[k] = (lag + i32::from(off)).max(min_lag).min(max_lag);
        }
        for k in 0..c.nb_subfr as usize {
            let i = c.ind.ltp_index[k] as usize;
            ctl.ltp_q7[k] = match c.ind.per_index {
                0 => SILK_LTP_TAPS0[i],
                1 => SILK_LTP_TAPS1[i],
                _ => SILK_LTP_TAPS2[i],
            };
        }
        const SCALES: [i32; 3] = [15565, 12288, 8192];
        ctl.ltp_scale_q14 = SCALES[c.ind.ltp_scale as usize];
    } else {
        ctl.pitch = [0; 4];
        ctl.ltp_q7 = [[0; 5]; 4];
        ctl.ltp_scale_q14 = 0;
    }
}

// Reconstruction uses the fixed-point formats of the RFC's reference decoder:
// the encoder quantizes against that exact filter state, so a decoder that
// rounds differently drifts audibly over long voiced stretches.

#[inline(always)]
fn smulwb(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b as i16)) >> 16) as i32
}

#[inline(always)]
fn smulww(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> 16) as i32
}

#[inline(always)]
fn rshift_round(a: i64, shift: u32) -> i32 {
    ((a + (1i64 << (shift - 1))) >> shift) as i32
}

#[inline(always)]
fn sat32(v: i64) -> i32 {
    crate::sample::sat32(v)
}

#[inline(always)]
fn lshift_sat(a: i32, shift: u32) -> i32 {
    sat32(i64::from(a) << shift)
}

fn inverse_varq(b: i32, qres: i32) -> i32 {
    let headroom = 32 - ilog(b.unsigned_abs()) - 1;
    let nrm = b << headroom;
    let inv = (i32::MAX >> 2) / (nrm >> 16);
    let result = inv << 16;
    let err_q32 = ((1 << 29) - smulwb(nrm, inv)).wrapping_shl(3);
    let result = result.wrapping_add(((i64::from(err_q32) * i64::from(inv)) >> 16) as i32);
    let shift = 61 - headroom - qres;
    if shift <= 0 {
        return lshift_sat(result, -shift as u32);
    }
    if shift < 32 { result >> shift } else { 0 }
}

fn div_varq(a: i32, b: i32, qres: i32) -> i32 {
    let a_head = 32 - ilog(a.unsigned_abs()) - 1;
    let mut a_nrm = a << a_head;
    let b_head = 32 - ilog(b.unsigned_abs()) - 1;
    let b_nrm = b << b_head;
    let inv = (i32::MAX >> 2) / (b_nrm >> 16);
    let mut result = smulwb(a_nrm, inv);
    let back = (((i64::from(b_nrm) * i64::from(result)) >> 32) as i32 as u32) << 3;
    a_nrm = (a_nrm as u32).wrapping_sub(back) as i32;
    result = result.wrapping_add(smulwb(a_nrm, inv));
    let shift = 29 + a_head - b_head - qres;
    if shift < 0 {
        return lshift_sat(result, -shift as u32);
    }
    if shift < 32 { result >> shift } else { 0 }
}

// out[i] for d <= i < len is in[i] minus its prediction from in[i - d .. i - 1].
unsafe fn analysis_filter(out: *mut i16, input: *const i16, a_q12: &[i16; MAX_ORDER], len: usize, d: usize) {
    unsafe {
        for i in d..len {
            let mut acc = 0u32;
            for k in 0..d {
                acc = acc.wrapping_add((i32::from(*input.add(i - k - 1)) * i32::from(a_q12[k])) as u32);
            }
            let v = ((*input.add(i) as i32 as u32) << 12).wrapping_sub(acc) as i32;
            *out.add(i) = sat16(rshift_round(i64::from(v), 12));
        }
        core::ptr::write_bytes(out, 0, d);
    }
}

fn excitation(w: &mut Work, c: &Channel) {
    const OFFSETS_Q10: [[i32; 2]; 3] = [[100, 240], [100, 240], [32, 100]];
    let offset = OFFSETS_Q10[c.ind.signal_type as usize][c.ind.offset_type as usize] << 4;
    let mut seed = c.ind.seed as u32;
    for i in 0..c.frame_len as usize {
        let raw = w.pulses[i];
        let mut v = raw * (1 << 14);
        if v > 0 {
            v -= 80 << 4;
        }
        if v < 0 {
            v += 80 << 4;
        }
        v += offset;
        seed = 907633515u32.wrapping_add(seed.wrapping_mul(196314165));
        if seed & 0x80000000 != 0 {
            v = -v;
        }
        w.exc_q14[i] = v;
        seed = seed.wrapping_add(raw as u32);
    }
}

fn synthesize(w: &mut Work, c: &mut Channel, ctl: &mut FrameCtrl, signal_type: i32, interp: bool, pcm: &mut [i16]) {
    let n = c.subfr_len as usize;
    let d = c.order as usize;
    let ltp_mem = 20 * c.fs_khz as usize;
    let mut ltp_index = ltp_mem;
    assert!(n * c.nb_subfr as usize <= pcm.len().min(MAX_FRAME) && n <= MAX_SUBFR && d <= MAX_ORDER);
    w.lpc_q14[..MAX_ORDER].copy_from_slice(&c.lpc_q14);
    let (lpc, exc, res, out) = (w.lpc_q14.as_mut_ptr(), w.exc_q14.as_ptr(), w.res.as_mut_ptr(), pcm.as_mut_ptr());

    for k in 0..c.nb_subfr as usize {
        let a = &ctl.a_q12[k >> 1];
        let b = &mut ctl.ltp_q7[k];
        let mut kind = signal_type;
        let gain_q16 = ctl.gain_q16[k];
        let gain_q10 = gain_q16 >> 6;
        let mut inv_gain_q31 = inverse_varq(gain_q16, 47);
        let mut gain_adj_q16 = 1 << 16;
        if gain_q16 != c.prev_gain_q16 {
            gain_adj_q16 = div_varq(c.prev_gain_q16, gain_q16, 16);
            for v in &mut w.lpc_q14[..MAX_ORDER] {
                *v = smulww(gain_adj_q16, *v);
            }
        }
        c.prev_gain_q16 = gain_q16;

        if c.loss_count != 0 && c.prev_signal_type == TYPE_VOICED && kind != TYPE_VOICED && k < 2 {
            *b = [0, 0, 32, 0, 0];
            kind = TYPE_VOICED;
            ctl.pitch[k] = c.lag_prev;
        }

        let voiced = kind == TYPE_VOICED;
        if voiced {
            let lag = ctl.pitch[k] as usize;
            if k == 0 || (k == 2 && interp) {
                let start = ltp_mem - lag - d - 2;
                if k == 2 {
                    c.out[ltp_mem..ltp_mem + 2 * n].copy_from_slice(&pcm[..2 * n]);
                }
                unsafe { analysis_filter(w.ltp.as_mut_ptr().add(start), c.out.as_ptr().add(start + k * n), a, ltp_mem - start, d) };
                if k == 0 {
                    inv_gain_q31 = smulwb(inv_gain_q31, ctl.ltp_scale_q14).wrapping_shl(2);
                }
                for i in 0..lag + 2 {
                    w.ltp_q15[ltp_index - i - 1] = smulwb(inv_gain_q31, i32::from(w.ltp[ltp_mem - i - 1]));
                }
            } else if gain_adj_q16 != 1 << 16 {
                for i in 0..lag + 2 {
                    let at = ltp_index - i - 1;
                    w.ltp_q15[at] = smulww(gain_adj_q16, w.ltp_q15[at]);
                }
            }
            let pred = ltp_index + 2 - lag;
            let ltp = w.ltp_q15.as_mut_ptr();
            let tap = |j: usize| i32::from(b[j]) << 7;
            for i in 0..n {
                unsafe {
                    let mut p = 2i32;
                    for j in 0..5 {
                        p = p.wrapping_add(smulwb(*ltp.add(pred + i - j), tap(j)));
                    }
                    let r = (*exc.add(k * n + i)).wrapping_add(p.wrapping_mul(2));
                    *res.add(i) = r;
                    *ltp.add(ltp_index) = r.wrapping_mul(2);
                }
                ltp_index += 1;
            }
        }

        let excitation = if voiced { res.cast_const() } else { unsafe { exc.add(k * n) } };
        for i in 0..n {
            unsafe {
                let mut p = (d >> 1) as i32;
                let top = lpc.add(MAX_ORDER + i);
                for j in 0..d {
                    p = p.wrapping_add(smulwb(*top.sub(j + 1), i32::from(*a.get_unchecked(j))));
                }
                let v = sat32(i64::from(*excitation.add(i)) + i64::from(lshift_sat(p, 4)));
                *top = v;
                *out.add(k * n + i) = sat16(rshift_round(i64::from(smulww(v, gain_q10)), 8));
            }
        }
        w.lpc_q14.copy_within(n..n + MAX_ORDER, 0);
    }
    c.lpc_q14.copy_from_slice(&w.lpc_q14[..MAX_ORDER]);

    let frame_len = c.frame_len as usize;
    let keep = ltp_mem - frame_len;
    c.out.copy_within(frame_len..frame_len + keep, 0);
    c.out[keep..keep + frame_len].copy_from_slice(&pcm[..frame_len]);
}

// Not specified by RFC 6716: the last frame's filters run on again, voiced with
// its pitch predictor and no new excitation, otherwise on noise at the last
// excitation level, fading out frame by frame.
fn conceal(w: &mut Work, c: &mut Channel, pcm: &mut [i16]) {
    let mut ctl = c.last;
    let fade = if c.loss_count == 0 { 58982i64 } else { 45875 };
    let tail = if c.nb_subfr == 4 { 3 } else { 1 };
    for k in 0..4 {
        ctl.gain_q16[k] = c.last.gain_q16[tail];
        ctl.pitch[k] = c.lag_prev;
        ctl.ltp_q7[k] = c.last.ltp_q7[tail];
    }
    ctl.a_q12[0] = c.last.a_q12[1];
    ctl.ltp_scale_q14 = 16384;
    let level = c.last_exc_q14;
    let voiced = c.prev_signal_type == TYPE_VOICED;
    for i in 0..c.frame_len as usize {
        c.plc_seed = 907633515u32.wrapping_add(c.plc_seed.wrapping_mul(196314165));
        w.exc_q14[i] = if voiced { 0 } else { ((i64::from(c.plc_seed as i32) * i64::from(level)) >> 31) as i32 };
    }
    if voiced {
        for k in 0..4 {
            for j in 0..5 {
                ctl.ltp_q7[k][j] = ((i64::from(ctl.ltp_q7[k][j]) * fade) >> 16) as i8;
            }
        }
    }
    let mut chirp = 64880i32;
    for i in 0..c.order as usize {
        let v = rshift_round(i64::from(chirp) * i64::from(ctl.a_q12[1][i]), 16) as i16;
        ctl.a_q12[0][i] = v;
        ctl.a_q12[1][i] = v;
        chirp += rshift_round(i64::from(chirp) * i64::from(chirp - 65536), 16);
    }
    c.last = ctl;
    for k in 0..4 {
        c.last.gain_q16[k] = ((i64::from(ctl.gain_q16[k]) * fade) >> 16) as i32;
    }
    c.last_exc_q14 = ((i64::from(level) * fade) >> 16) as i32;
    let kind = if voiced { TYPE_VOICED } else { TYPE_UNVOICED };
    synthesize(w, c, &mut ctl, kind, false, pcm);
    c.loss_count += 1;
}

fn decode_frame(w: &mut Work, c: &mut Channel, rc: Option<&mut Rc<'_>>, cond: i32, pcm: &mut [i16]) -> i32 {
    if let Some(rc) = rc {
        let mut ctl = c.last;
        let frame = c.frames_decoded as usize;
        decode_indices(c, rc, frame, false, cond);
        decode_pulses(rc, &mut w.pulses, c.ind.signal_type, c.ind.offset_type, c.frame_len);
        decode_parameters(c, &mut ctl, cond);
        excitation(w, c);
        let interp = c.nb_subfr == 4 && c.ind.interp < 4;
        synthesize(w, c, &mut ctl, c.ind.signal_type, interp, pcm);
        let frame_len = c.frame_len as usize;
        let energy: i64 = w.exc_q14[..frame_len].iter().map(|&v| i64::from(v >> 4) * i64::from(v >> 4)).sum();
        c.last_exc_q14 = (sqrtf(energy as f32 / frame_len as f32) as i32) << 4;
        c.last = ctl;
        c.loss_count = 0;
        c.prev_signal_type = c.ind.signal_type;
        c.first_frame_after_reset = false;
        c.lag_prev = ctl.pitch[c.nb_subfr as usize - 1];
    } else {
        conceal(w, c, pcm);
    }
    c.frame_len
}

fn decode_stereo_pred(rc: &mut Rc<'_>, pred_q13: &mut [i32; 2]) {
    let n = rc.icdf(&SILK_STEREO_S1_ICDF, 8);
    let mut ix = [[0i32; 3]; 2];
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for v in &mut ix {
        v[0] = rc.icdf(&SILK_STEREO_S2_ICDF, 8);
        v[1] = rc.icdf(&SILK_STEREO_S3_ICDF, 8);
    }
    for i in 0..2 {
        let wi = (ix[i][0] + 3 * ix[i][2]) as usize;
        let low = i32::from(SILK_STEREO_WEIGHTS[wi]);
        let step = ((i32::from(SILK_STEREO_WEIGHTS[wi + 1]) - low) * 6554) >> 16;
        pred_q13[i] = low + step * (2 * ix[i][1] + 1);
    }
    pred_q13[0] -= pred_q13[1];
}

impl Silk {
    fn ms_to_lr(&mut self, pred_q13: &[i32; 2], fs_khz: i32, len: usize) {
        let (m, s) = self.buf.split_at_mut(1);
        let (mid, side) = (&mut m[0], &mut s[0]);
        mid[..2].copy_from_slice(&self.s_mid);
        side[..2].copy_from_slice(&self.s_side);
        self.s_mid.copy_from_slice(&mid[len..len + 2]);
        self.s_side.copy_from_slice(&side[len..len + 2]);
        let n1 = 8 * fs_khz as usize;
        let denom_q16 = (1i32 << 16) / n1 as i32;
        let delta = |p: i32, prev: i32| rshift_round(i64::from((p - prev) as i16) * i64::from(denom_q16 as i16), 16);
        let delta0 = delta(pred_q13[0], self.pred_prev_q13[0]);
        let delta1 = delta(pred_q13[1], self.pred_prev_q13[1]);
        let mut w0 = self.pred_prev_q13[0];
        let mut w1 = self.pred_prev_q13[1];
        for i in 0..len {
            if i < n1 {
                w0 += delta0;
                w1 += delta1;
            } else {
                w0 = pred_q13[0];
                w1 = pred_q13[1];
            }
            let p0 = (i32::from(mid[i]) + i32::from(mid[i + 2]) + 2 * i32::from(mid[i + 1])) << 9;
            let mut v = (i32::from(side[i + 1]) << 8).wrapping_add(smulwb(p0, w0));
            v = v.wrapping_add(smulwb(i32::from(mid[i + 1]) << 11, w1));
            side[i + 1] = sat16(rshift_round(i64::from(v), 8));
        }
        self.pred_prev_q13 = *pred_q13;
        for i in 1..=len {
            let (m, sd) = (i32::from(mid[i]), i32::from(side[i]));
            mid[i] = sat16(m + sd);
            side[i] = sat16(m - sd);
        }
    }

    pub fn init(&mut self, channels: i32) {
        self.channels = channels;
        self.reset();
    }

    pub fn reset(&mut self) {
        let channels = self.channels;
        unsafe { core::ptr::write_bytes(core::ptr::from_mut(self), 0, 1) };
        self.channels = channels;
        channel_init(&mut self.ch[0]);
        channel_init(&mut self.ch[1]);
    }

    /// Decodes the next 10 or 20 ms SILK frame of a packet of `payload_ms` (the
    /// call with `first` set also reads the packet's VAD and LBRR flags) and
    /// resamples it to 48 kHz into `out`, interleaved; returns the samples per
    /// channel written. `rc` `None` conceals instead of reading.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn decode(&mut self, rc: Option<&mut Rc<'_>>, first: bool, internal_khz: i32, stream_channels: usize, payload_ms: i32, out: *mut i16) -> i32 {
        let lost = rc.is_none();
        let mut rc = rc;
        let mut internal_khz = internal_khz;
        let mut decode_only_middle = 0;
        let mut pred_q13 = [0i32; 2];
        let streams = stream_channels as i32;

        if lost && self.ch[0].fs_khz != 0 {
            internal_khz = self.ch[0].fs_khz;
        }
        if first {
            for c in &mut self.ch[..stream_channels] {
                c.frames_decoded = 0;
            }
        }
        if streams > self.channels_internal {
            channel_init(&mut self.ch[1]);
        }
        let stereo_to_mono = streams == 1 && self.channels_internal == 2 && internal_khz == self.ch[0].fs_khz;
        if self.ch[0].frames_decoded == 0 {
            for c in &mut self.ch[..stream_channels] {
                c.frames_per_packet = if payload_ms <= 20 { 1 } else { payload_ms / 20 };
                c.nb_subfr = if payload_ms == 10 { 2 } else { 4 };
                channel_set_fs(c, internal_khz);
            }
        }
        if self.channels == 2 && streams == 2 && self.channels_internal == 1 {
            self.pred_prev_q13 = [0; 2];
            self.s_side = [0; 2];
            self.ch[1].rs = self.ch[0].rs;
        }
        self.channels_internal = streams;

        if let Some(rc) = rc.as_deref_mut() {
            if self.ch[0].frames_decoded == 0 {
                for c in &mut self.ch[..stream_channels] {
                    for i in 0..c.frames_per_packet as usize {
                        c.vad[i] = i32::from(rc.bit_logp(1));
                    }
                    c.lbrr_flag = i32::from(rc.bit_logp(1));
                }
                for c in &mut self.ch[..stream_channels] {
                    c.lbrr = [0; 3];
                    if c.lbrr_flag == 0 {
                        continue;
                    }
                    if c.frames_per_packet == 1 {
                        c.lbrr[0] = 1;
                    } else {
                        let sym = rc.icdf(&SILK_LBRR_ICDF[c.frames_per_packet as usize - 2], 8) + 1;
                        for i in 0..c.frames_per_packet as usize {
                            c.lbrr[i] = (sym >> i) & 1;
                        }
                    }
                }
                for i in 0..self.ch[0].frames_per_packet as usize {
                    for n in 0..stream_channels {
                        if self.ch[n].lbrr[i] == 0 {
                            continue;
                        }
                        if stream_channels == 2 && n == 0 {
                            decode_stereo_pred(rc, &mut pred_q13);
                            if self.ch[1].lbrr[i] == 0 {
                                decode_only_middle = rc.icdf(&SILK_MID_ONLY_ICDF, 8);
                            }
                        }
                        let c = &mut self.ch[n];
                        let cond = if i > 0 && c.lbrr[i - 1] != 0 { CODE_CONDITIONALLY } else { CODE_INDEPENDENTLY };
                        decode_indices(c, rc, i, true, cond);
                        decode_pulses(rc, &mut self.w.pulses, c.ind.signal_type, c.ind.offset_type, c.frame_len);
                    }
                }
            }
        }

        if stream_channels == 2 {
            if let Some(rc) = rc.as_deref_mut() {
                decode_stereo_pred(rc, &mut pred_q13);
                let at = self.ch[0].frames_decoded as usize;
                decode_only_middle = if self.ch[1].vad[at] == 0 { rc.icdf(&SILK_MID_ONLY_ICDF, 8) } else { 0 };
            } else {
                pred_q13 = self.pred_prev_q13;
            }
        }
        if stream_channels == 2 && decode_only_middle == 0 && self.prev_decode_only_middle == 1 {
            let side = &mut self.ch[1];
            side.out = [0; LTP_MEM + 2 * MAX_SUBFR];
            side.lpc_q14 = [0; MAX_ORDER];
            side.lag_prev = 100;
            side.last_gain_index = 10;
            side.prev_signal_type = TYPE_INACTIVE;
            side.first_frame_after_reset = true;
        }
        let has_side = if lost { self.prev_decode_only_middle == 0 } else { decode_only_middle == 0 };

        let mut frame_len = self.ch[0].frame_len;
        for n in 0..stream_channels {
            if n == 0 || has_side {
                let index = self.ch[0].frames_decoded - n as i32;
                let cond = if index <= 0 {
                    CODE_INDEPENDENTLY
                } else if n > 0 && self.prev_decode_only_middle != 0 {
                    CODE_INDEPENDENTLY_NO_LTP_SCALING
                } else {
                    CODE_CONDITIONALLY
                };
                frame_len = decode_frame(&mut self.w, &mut self.ch[n], rc.as_deref_mut(), cond, &mut self.buf[n][2..]);
            } else {
                self.buf[n][2..2 + frame_len as usize].fill(0);
            }
            self.ch[n].frames_decoded += 1;
        }

        let len = frame_len as usize;
        if self.channels == 2 && streams == 2 {
            let fs = self.ch[0].fs_khz;
            self.ms_to_lr(&pred_q13, fs, len);
        } else {
            self.buf[0][..2].copy_from_slice(&self.s_mid);
            self.s_mid.copy_from_slice(&self.buf[0][len..len + 2]);
        }

        let out_len = (frame_len * 48 / self.ch[0].fs_khz) as usize;
        let ch_out = self.channels as usize;
        let up = self.w.upsampled.as_mut_ptr();
        let res = self.w.resampled.as_mut_ptr();
        unsafe {
            for n in 0..ch_out.min(stream_channels) {
                resample(&mut self.ch[n].rs, up, res, self.buf[n].as_ptr().add(1), frame_len);
                for i in 0..out_len {
                    *out.add(i * ch_out + n) = *res.add(i);
                }
            }
            if ch_out == 2 && stream_channels == 1 {
                if stereo_to_mono {
                    resample(&mut self.ch[1].rs, up, res, self.buf[0].as_ptr().add(1), frame_len);
                    for i in 0..out_len {
                        *out.add(i * 2 + 1) = *res.add(i);
                    }
                } else {
                    for i in 0..out_len {
                        *out.add(i * 2 + 1) = *out.add(i * 2);
                    }
                }
            }
        }

        if lost {
            for c in &mut self.ch[..self.channels_internal as usize] {
                c.last_gain_index = 10;
            }
        } else {
            self.prev_decode_only_middle = decode_only_middle;
        }
        out_len as i32
    }
}
