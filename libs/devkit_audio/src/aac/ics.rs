/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::math::{ldexpf, lrintf, sqrtf};

use super::tables::*;
use super::{AAC_HCB_SF, Chan, Dec, EIGHT_SHORT, FRAME_LEN, Ics, MAX_SFB, huff};
use crate::bits::Bits;
use crate::{Error, Result};

const ZERO_HCB: u8 = 0;
const ESC_HCB: i32 = 11;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

const SPEC_LIMIT: i32 = 1 << 29;
const SPEC_SCALE: i32 = 16;

pub(super) fn ics_info(sf_index: usize, b: &mut Bits<'_>, ics: &mut Ics) -> Result<()> {
    b.bit();
    ics.window_sequence = b.get(2) as u8;
    ics.window_shape = b.bit();
    if ics.window_sequence == EIGHT_SHORT {
        ics.max_sfb = b.get(4) as u8;
        let grouping = b.get(7);
        ics.num_windows = 8;
        ics.num_groups = 1;
        ics.group_len[0] = 1;
        for i in (0..7).rev() {
            if grouping >> i & 1 != 0 {
                ics.group_len[usize::from(ics.num_groups) - 1] += 1;
            } else {
                ics.group_len[usize::from(ics.num_groups)] = 1;
                ics.num_groups += 1;
            }
        }
        ics.num_swb = AAC_NUM_SWB_SHORT[sf_index];
        ics.swb_offset = AAC_SWB_OFFSET_SHORT[sf_index];
    } else {
        ics.max_sfb = b.get(6) as u8;
        if b.bit() {
            return Err(Error::NotSupported);
        }
        ics.num_windows = 1;
        ics.num_groups = 1;
        ics.group_len[0] = 1;
        ics.num_swb = AAC_NUM_SWB_LONG[sf_index];
        ics.swb_offset = AAC_SWB_OFFSET_LONG[sf_index];
    }
    if ics.max_sfb <= ics.num_swb { Ok(()) } else { Err(Error::InvalidSize) }
}

fn section_data(b: &mut Bits<'_>, ics: &mut Ics) -> Result<()> {
    let bits = if ics.window_sequence == EIGHT_SHORT { 3 } else { 5 };
    let esc = (1u32 << bits) - 1;
    let max_sfb = usize::from(ics.max_sfb);
    for g in 0..usize::from(ics.num_groups) {
        let mut k = 0;
        while k < max_sfb {
            let cb = b.get(4) as u8;
            if cb == 12 {
                return Err(Error::InvalidData);
            }
            let mut len = 0;
            loop {
                let incr = b.get(bits);
                len += incr as usize;
                if b.overrun() {
                    return Err(Error::InvalidSize);
                }
                if incr != esc {
                    break;
                }
            }
            if k + len > max_sfb {
                return Err(Error::InvalidSize);
            }
            ics.cb[g][k..k + len].fill(cb);
            k += len;
        }
    }
    Ok(())
}

fn scale_factor_data(b: &mut Bits<'_>, ics: &mut Ics) -> Result<()> {
    let mut sf = i32::from(ics.global_gain);
    let mut noise = sf - 90;
    let mut is = 0;
    let mut noise_pcm = true;
    for g in 0..usize::from(ics.num_groups) {
        for sfb in 0..usize::from(ics.max_sfb) {
            ics.sf[g][sfb] = match ics.cb[g][sfb] {
                ZERO_HCB => 0,
                INTENSITY_HCB | INTENSITY_HCB2 => {
                    is += huff(b, AAC_HCB_SF);
                    if !(-155..=100).contains(&is) {
                        return Err(Error::InvalidData);
                    }
                    is as i16
                }
                NOISE_HCB => {
                    if noise_pcm {
                        noise_pcm = false;
                        noise += b.get(9) as i32 - 256;
                    } else {
                        noise += huff(b, AAC_HCB_SF);
                    }
                    if !(-100..=155).contains(&noise) {
                        return Err(Error::InvalidData);
                    }
                    noise as i16
                }
                _ => {
                    sf += huff(b, AAC_HCB_SF);
                    if !(0..=255).contains(&sf) {
                        return Err(Error::InvalidData);
                    }
                    sf as i16
                }
            };
        }
    }
    Ok(())
}

fn pulse_data(b: &mut Bits<'_>, ics: &mut Ics) -> Result<()> {
    ics.pulse_n = b.get(2) as u8 + 1;
    ics.pulse_start = b.get(6) as u8;
    if ics.pulse_start >= ics.num_swb {
        return Err(Error::InvalidData);
    }
    for i in 0..usize::from(ics.pulse_n) {
        ics.pulse_offset[i] = b.get(5) as u8;
        ics.pulse_amp[i] = b.get(4) as u8;
    }
    Ok(())
}

fn tns_data(b: &mut Bits<'_>, ics: &mut Ics) -> Result<()> {
    let is8 = ics.window_sequence == EIGHT_SHORT;
    let max_order = if is8 { 7 } else { 12 };
    for w in 0..usize::from(ics.num_windows) {
        let t = &mut ics.tns_win[w];
        t.n_filt = b.get(if is8 { 1 } else { 2 }) as u8;
        if t.n_filt != 0 {
            t.coef_res = b.bit();
        }
        for f in 0..usize::from(t.n_filt) {
            t.length[f] = b.get(if is8 { 4 } else { 6 }) as u8;
            t.order[f] = b.get(if is8 { 3 } else { 5 }) as u8;
            if t.order[f] > max_order {
                return Err(Error::InvalidData);
            }
            if t.order[f] == 0 {
                continue;
            }
            t.direction[f] = b.bit();
            t.compress[f] = b.bit();
            let nbits = 3 + u32::from(t.coef_res) - u32::from(t.compress[f]);
            for i in 0..usize::from(t.order[f]) {
                t.coef[f][i] = b.get(nbits) as u8;
            }
        }
    }
    Ok(())
}

fn escape(b: &mut Bits<'_>) -> Option<i32> {
    let mut n = 4;
    while b.bit() {
        n += 1;
        if n > 12 {
            return None;
        }
    }
    Some((1 << n) + b.get(n) as i32)
}

fn decode_band(b: &mut Bits<'_>, cb: i32, dst: &mut [i32]) -> Result<()> {
    let table = (cb - 1) as usize;
    match cb {
        1 | 2 => {
            for d in dst.chunks_exact_mut(4) {
                let v = huff(b, table);
                d[0] = v / 27 - 1;
                d[1] = v / 9 % 3 - 1;
                d[2] = v / 3 % 3 - 1;
                d[3] = v % 3 - 1;
            }
        }
        3 | 4 => {
            for d in dst.chunks_exact_mut(4) {
                let v = huff(b, table);
                let q = [v / 27, v / 9 % 3, v / 3 % 3, v % 3];
                for (d, &q) in d.iter_mut().zip(&q) {
                    *d = if q != 0 && b.bit() { -q } else { q };
                }
            }
        }
        5 | 6 => {
            for d in dst.chunks_exact_mut(2) {
                let v = huff(b, table);
                d[0] = v / 9 - 4;
                d[1] = v % 9 - 4;
            }
        }
        _ => {
            let modulus = match cb {
                ..=8 => 8,
                ..=10 => 13,
                _ => 17,
            };
            for d in dst.chunks_exact_mut(2) {
                let v = huff(b, table);
                let (mut y, mut z) = (v / modulus, v % modulus);
                let ny = y != 0 && b.bit();
                let nz = z != 0 && b.bit();
                if cb == ESC_HCB {
                    if y == 16 {
                        y = escape(b).ok_or(Error::InvalidData)?;
                    }
                    if z == 16 {
                        z = escape(b).ok_or(Error::InvalidData)?;
                    }
                }
                d[0] = if ny { -y } else { y };
                d[1] = if nz { -z } else { z };
            }
        }
    }
    if b.overrun() { Err(Error::InvalidSize) } else { Ok(()) }
}

fn spectral_data(b: &mut Bits<'_>, c: &mut Chan) -> Result<()> {
    let ics = &c.ics;
    let swb = ics.swb_offset;
    let stride = if ics.num_windows == 8 { 128 } else { 1024 };
    c.spec.fill(0);
    let mut win = 0;
    for g in 0..usize::from(ics.num_groups) {
        for sfb in 0..usize::from(ics.max_sfb) {
            let cb = ics.cb[g][sfb];
            if cb == ZERO_HCB || cb >= NOISE_HCB {
                continue;
            }
            let (lo, hi) = (usize::from(swb[sfb]), usize::from(swb[sfb + 1]));
            for w in 0..usize::from(ics.group_len[g]) {
                let at = (win + w) * stride;
                decode_band(b, i32::from(cb), &mut c.spec[at + lo..at + hi])?;
            }
        }
        win += usize::from(ics.group_len[g]);
    }
    Ok(())
}

pub(super) fn ics_parse(sf_index: usize, b: &mut Bits<'_>, c: &mut Chan, common_window: bool) -> Result<()> {
    let ics = &mut c.ics;
    ics.global_gain = b.get(8) as u8;
    if !common_window {
        ics_info(sf_index, b, ics)?;
    }
    section_data(b, ics)?;
    scale_factor_data(b, ics)?;
    ics.pulse = b.bit();
    if ics.pulse {
        if ics.window_sequence == EIGHT_SHORT {
            return Err(Error::InvalidData);
        }
        pulse_data(b, ics)?;
    }
    ics.tns = b.bit();
    if ics.tns {
        tns_data(b, ics)?;
    }
    if b.bit() {
        return Err(Error::NotSupported);
    }
    if b.overrun() {
        return Err(Error::InvalidSize);
    }
    spectral_data(b, c)
}

fn pow43_q13(q: i32) -> i32 {
    if q < 16 {
        return AAC_POW43_Q13[q as usize];
    }
    let target = (q as u64) << 48;
    let (mut lo, mut hi) = (0u32, 1u32 << 21);
    while lo < hi {
        let mid = (lo + hi + 1) >> 1;
        if u64::from(mid) * u64::from(mid) * u64::from(mid) <= target {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (((q as u64) * u64::from(lo)) >> 3) as i32
}

#[inline(always)]
fn clamp_spec(v: i64) -> i32 {
    v.clamp(-i64::from(SPEC_LIMIT), i64::from(SPEC_LIMIT)) as i32
}

fn shift_spec(v: i64, left: i32) -> i32 {
    if left >= 0 {
        if left > 30 {
            return match v {
                0 => 0,
                1.. => SPEC_LIMIT,
                _ => -SPEC_LIMIT,
            };
        }
        return clamp_spec(v * (1i64 << left));
    }
    if left < -62 {
        return 0;
    }
    clamp_spec(v >> -left)
}

const POW2_QUARTER_Q14: [i16; 4] = [16384, 19484, 23170, 27554];
const POW2_NEG_QUARTER_Q16: [i32; 4] = [0, 55109, 46341, 38968];
const POW2_QUARTER: [f32; 4] = [1.0, 1.18920712, 1.41421356, 1.68179283];

fn dequant_band(x: &mut [i32], sf: i32) {
    let e = sf - 100 + SPEC_SCALE;
    let shift = (e >> 2) - 27;
    let frac = i64::from(POW2_QUARTER_Q14[(e & 3) as usize]);
    for v in x {
        let q = *v;
        if q != 0 {
            let m = shift_spec(i64::from(pow43_q13(q.abs())) * frac, shift);
            *v = if q < 0 { -m } else { m };
        }
    }
}

fn lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed
}

fn noise_band(x: &mut [i32], nrg: i32, seed: &mut u32) {
    let mut energy = 0i64;
    for v in x.iter_mut() {
        *v = (lcg(seed) as i32) >> 16;
        energy += i64::from(*v) * i64::from(*v);
    }
    if energy == 0 {
        return;
    }
    let e = nrg + SPEC_SCALE;
    let scale = POW2_QUARTER[(e & 3) as usize] * ldexpf(1.0, e >> 2) / sqrtf(energy as f32);
    for v in x {
        *v = clamp_spec((*v as f32 * scale) as i64);
    }
}

fn intensity_band(l: &[i32], r: &mut [i32], pos: i32, sign: i32) {
    let a = pos >> 2;
    let frac = POW2_NEG_QUARTER_Q16[(pos & 3) as usize];
    for (l, r) in l.iter().zip(r) {
        let mut v = if frac != 0 { ((i64::from(*l) * i64::from(frac)) >> 16) as i32 } else { *l };
        v = if a >= 0 {
            if a > 31 { 0 } else { v >> a }
        } else {
            shift_spec(i64::from(v), -a)
        };
        *r = if sign < 0 { -v } else { v };
    }
}

fn apply_tns(sf_index: usize, scratch: *mut i32, c: &mut Chan) {
    let ics = &c.ics;
    let is8 = ics.num_windows == 8;
    let tns_max = if is8 { AAC_TNS_MAX_BANDS_SHORT[sf_index] } else { AAC_TNS_MAX_BANDS_LONG[sf_index] };
    let mmm = usize::from(tns_max.min(ics.max_sfb));
    let swb = ics.swb_offset;
    let y = scratch.cast::<f32>();
    for w in 0..usize::from(ics.num_windows) {
        let t = &ics.tns_win[w];
        let spec = &mut c.spec[w * 128..];
        let mut bottom = i32::from(ics.num_swb);
        for f in 0..usize::from(t.n_filt) {
            let top = bottom;
            bottom = (top - i32::from(t.length[f])).max(0);
            let order = usize::from(t.order[f]);
            if order == 0 {
                continue;
            }
            let map = &AAC_TNS_COEF[usize::from(t.coef_res) * 2 + usize::from(t.compress[f])];
            let mut a = [0f64; 13];
            a[0] = 1.0;
            for m in 1..=order {
                let k = f64::from(map[usize::from(t.coef[f][m - 1])]);
                let mut tmp = [0f64; 13];
                for i in 1..m {
                    tmp[i] = a[i] + k * a[m - i];
                }
                a[1..m].copy_from_slice(&tmp[1..m]);
                a[m] = k;
            }
            let mut lpc = [0f32; 13];
            for i in 1..=order {
                lpc[i] = a[i] as f32;
            }
            let start = usize::from(swb[(bottom as usize).min(mmm)]);
            let end = usize::from(swb[(top as usize).min(mmm)]);
            if end <= start {
                continue;
            }
            let size = end - start;
            for m in 0..size {
                let at = if t.direction[f] { end - 1 - m } else { start + m };
                let mut acc = spec[at] as f32;
                for i in 1..m.min(order) + 1 {
                    acc -= lpc[i] * unsafe { *y.add(m - i) };
                }
                unsafe { *y.add(m) = acc };
                spec[at] = if acc.abs() >= 1e9 { clamp_spec(acc as i64) } else { clamp_spec(i64::from(lrintf(acc))) };
            }
        }
    }
}

fn channel_spectrum(
    ms_used: &[[u8; MAX_SFB + 1]; 8],
    noise_seed: &mut u32,
    noise_seeds: &mut [[u32; MAX_SFB + 1]; 8],
    left_cb: Option<&[[u8; MAX_SFB + 1]; 8]>,
    c: &mut Chan,
) {
    let ics = &c.ics;
    let swb = ics.swb_offset;
    let stride = if ics.num_windows == 8 { 128 } else { 1024 };
    if ics.pulse {
        let mut k = usize::from(swb[usize::from(ics.pulse_start)]);
        for i in 0..usize::from(ics.pulse_n) {
            k += usize::from(ics.pulse_offset[i]);
            if k >= FRAME_LEN {
                break;
            }
            let amp = i32::from(ics.pulse_amp[i]);
            c.spec[k] += if c.spec[k] > 0 { amp } else { -amp };
        }
    }
    let mut win = 0;
    for g in 0..usize::from(ics.num_groups) {
        for sfb in 0..usize::from(ics.max_sfb) {
            let cb = ics.cb[g][sfb];
            let (lo, hi) = (usize::from(swb[sfb]), usize::from(swb[sfb + 1]));
            let sf = i32::from(ics.sf[g][sfb]);
            if cb == NOISE_HCB {
                let correlated = left_cb.is_some_and(|l| ms_used[g][sfb] != 0 && l[g][sfb] == NOISE_HCB);
                let mut seed = if correlated { noise_seeds[g][sfb] } else { *noise_seed };
                if left_cb.is_none() {
                    noise_seeds[g][sfb] = *noise_seed;
                }
                for w in 0..usize::from(ics.group_len[g]) {
                    let at = (win + w) * stride;
                    noise_band(&mut c.spec[at + lo..at + hi], sf, &mut seed);
                }
                if !correlated {
                    *noise_seed = seed;
                }
            } else if cb != ZERO_HCB && cb < NOISE_HCB {
                for w in 0..usize::from(ics.group_len[g]) {
                    let at = (win + w) * stride;
                    dequant_band(&mut c.spec[at + lo..at + hi], sf);
                }
            }
        }
        win += usize::from(ics.group_len[g]);
    }
}

impl Dec {
    pub(super) fn spectrum(&mut self) {
        let channels = self.core_channels;
        let (left, right) = self.ch.split_at_mut(1);
        channel_spectrum(&self.ms_used, &mut self.noise_seed, &mut self.noise_seeds, None, &mut left[0]);
        if channels == 2 {
            let left_cb = Some(&left[0].ics.cb);
            channel_spectrum(&self.ms_used, &mut self.noise_seed, &mut self.noise_seeds, left_cb, &mut right[0]);
        }
        if channels == 2 && self.common_window {
            let (l, r) = (&mut left[0], &mut right[0]);
            let ics = &l.ics;
            let rics = &r.ics;
            let swb = ics.swb_offset;
            let stride = if ics.num_windows == 8 { 128 } else { 1024 };
            let mut win = 0;
            for g in 0..usize::from(ics.num_groups) {
                for sfb in 0..usize::from(ics.max_sfb) {
                    let (lo, hi) = (usize::from(swb[sfb]), usize::from(swb[sfb + 1]));
                    let rcb = rics.cb[g][sfb];
                    let ms = self.ms_used[g][sfb] != 0;
                    for w in 0..usize::from(ics.group_len[g]) {
                        let at = (win + w) * stride;
                        let ls = &mut l.spec[at + lo..at + hi];
                        let rs = &mut r.spec[at + lo..at + hi];
                        if rcb == INTENSITY_HCB || rcb == INTENSITY_HCB2 {
                            let mut sign = if rcb == INTENSITY_HCB { 1 } else { -1 };
                            if ms {
                                sign = -sign;
                            }
                            intensity_band(ls, rs, i32::from(rics.sf[g][sfb]), sign);
                        } else if ms && ics.cb[g][sfb] < NOISE_HCB && rcb < NOISE_HCB {
                            for (l, r) in ls.iter_mut().zip(rs) {
                                let (m, s) = (i64::from(*l), i64::from(*r));
                                *l = clamp_spec(m + s);
                                *r = clamp_spec(m - s);
                            }
                        }
                    }
                }
                win += usize::from(ics.group_len[g]);
            }
        }
        let scratch = self.scratch.as_mut_ptr();
        for c in &mut self.ch {
            if c.ics.tns {
                apply_tns(self.sf_index, scratch, c);
            }
        }
    }
}
