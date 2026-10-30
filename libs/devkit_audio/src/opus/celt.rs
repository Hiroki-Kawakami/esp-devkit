/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ptr;

use devkit::Zeroable;
use devkit::math::lrintf;
use idf::heap::{self, Caps};

use super::bands::{BITRES, Bands, Pvq, anti_collapse, exp2, lcg, renormalise};
use super::mdct::Mdct;
use super::rc::Rc;
use super::tables::*;
use super::{CELT_ALLOC_VECTORS, CELT_BANDS, CELT_BUF, CELT_MAX_FRAME, CELT_OVERLAP, MAX_PVQ_N, NORM_ONE, SIG_SAT, SIG_SHIFT, mulh, sat_sig};
use crate::Result;

const FINE_OFFSET: i32 = 21;
const MAX_FINE_BITS: i32 = 8;
const ALLOC_STEPS: i32 = 6;
const SPREAD_NORMAL: i32 = 2;
const COMB_MIN_PERIOD: i32 = 15;
const PREEMPH_Q15: i32 = 27853;

const fn db(x: f32) -> i32 {
    (x * 1024.0) as i32
}

#[inline(always)]
fn eb(i: usize) -> i32 {
    i32::from(CELT_EBANDS[i])
}

#[repr(C)]
struct Alloc {
    pulses: [i32; CELT_BANDS],
    fine: [i32; CELT_BANDS],
    priority: [i32; CELT_BANDS],
    intensity: i32,
    dual_stereo: bool,
    balance: i32,
    coded_bands: i32,
}

// What one frame works on, kept off the stack.
#[repr(C)]
struct Frame {
    tf_res: [i32; CELT_BANDS],
    cap: [i32; CELT_BANDS],
    offsets: [i32; CELT_BANDS],
    bits1: [i32; CELT_BANDS],
    bits2: [i32; CELT_BANDS],
    thresh: [i32; CELT_BANDS],
    trim_offset: [i32; CELT_BANDS],
    collapse: [u8; 2 * CELT_BANDS],
    alloc: Alloc,
}

unsafe impl Zeroable for Frame {}

pub(super) struct Celt {
    channels: usize,
    pub stream_channels: u8,
    pub start: u8,
    pub end: u8,
    pub stereo_inversion: bool,
    rng: u32,
    loss_count: i32,
    pf_period: i32,
    pf_period_old: i32,
    pf_gain: i32,
    pf_gain_old: i32,
    pf_tapset: i32,
    pf_tapset_old: i32,
    preemph_mem: [i32; 2],
    old_e: [i32; 2 * CELT_BANDS],
    old_loge: [i32; 2 * CELT_BANDS],
    old_loge2: [i32; 2 * CELT_BANDS],
    background_loge: [i32; 2 * CELT_BANDS],
    mem: [Option<Box<[i32], Caps>>; 2],
    freq: Box<[i32], Caps>,
    x: Box<[i32], Caps>,
    norm: Box<[i32], Caps>,
    scratch: Box<[i32], Caps>,
    iy: Box<[i32], Caps>,
    pvq: Pvq,
    mdct: Mdct,
    frame: Box<Frame, Caps>,
}

fn laplace_decode(rc: &mut Rc<'_>, fs: u32, decay: i32) -> i32 {
    let mut fs = fs;
    let mut val = 0i32;
    let mut fl = 0u32;
    let fm = rc.decode_bin(15);
    if fm >= fs {
        val += 1;
        fl = fs;
        fs = (((32768 - 32 - fs) * (16384 - decay) as u32) >> 15) + 1;
        while fs > 1 && fm >= fl + 2 * fs {
            fs *= 2;
            fl += fs;
            fs = (((fs - 2) * decay as u32) >> 15) + 1;
            val += 1;
        }
        if fs <= 1 {
            let di = (fm - fl) >> 1;
            val += di as i32;
            fl += 2 * di;
        }
        if fm < fl + fs {
            val = -val;
        } else {
            fl += fs;
        }
    }
    rc.update(fl, (fl + fs).min(32768), 32768);
    val
}

#[allow(clippy::too_many_arguments)]
fn interp_bits2pulses(
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; CELT_BANDS],
    bits2: &[i32; CELT_BANDS],
    thresh: &[i32; CELT_BANDS],
    cap: &[i32; CELT_BANDS],
    total: i32,
    skip_rsv: i32,
    intensity_rsv: i32,
    dual_stereo_rsv: i32,
    c_count: i32,
    lm: i32,
    rc: &mut Rc<'_>,
    a: &mut Alloc,
) -> i32 {
    let (mut total, mut intensity_rsv, mut dual_stereo_rsv) = (total, intensity_rsv, dual_stereo_rsv);
    let alloc_floor = c_count << BITRES;
    let stereo = i32::from(c_count > 1);
    let log_m = lm << BITRES;
    let (mut lo, mut hi) = (0, 1 << ALLOC_STEPS);
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + (mid * bits2[j] >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut psum = 0;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + (lo * bits2[j] >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        a.pulses[j] = tmp;
        psum += tmp;
    }

    let mut coded = end;
    loop {
        let j = coded - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - psum;
        let percoeff = left / (eb(coded) - eb(start));
        left -= (eb(coded) - eb(start)) * percoeff;
        let rem = (left - (eb(j) - eb(start))).max(0);
        let band_width = eb(coded) - eb(j);
        let mut band_bits = a.pulses[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if rc.bit_logp(1) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= a.pulses[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(CELT_LOG2_FRAC[j - start]);
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            a.pulses[j] = alloc_floor;
        } else {
            a.pulses[j] = 0;
        }
        coded -= 1;
    }

    a.intensity = if intensity_rsv > 0 { start as i32 + rc.uint((coded + 1 - start) as u32) as i32 } else { 0 };
    if a.intensity <= start as i32 {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    a.dual_stereo = dual_stereo_rsv > 0 && rc.bit_logp(1);

    let mut left = total - psum;
    let percoeff = left / (eb(coded) - eb(start));
    left -= (eb(coded) - eb(start)) * percoeff;
    for j in start..coded {
        a.pulses[j] += percoeff * (eb(j + 1) - eb(j));
    }
    for j in start..coded {
        let tmp = left.min(eb(j + 1) - eb(j));
        a.pulses[j] += tmp;
        left -= tmp;
    }

    let mut balance = 0;
    let bits = &mut a.pulses;
    let ebits = &mut a.fine;
    for j in start..coded {
        let n0 = eb(j + 1) - eb(j);
        let n = n0 << lm;
        let mut excess;
        bits[j] += balance;
        if n > 1 {
            excess = (bits[j] - cap[j]).max(0);
            bits[j] -= excess;
            let den = c_count * n + i32::from(c_count == 2 && n > 2 && !a.dual_stereo && (j as i32) < a.intensity);
            let nclogn = den * (i32::from(CELT_LOG_N[j]) + log_m);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if bits[j] + offset < den * 2 << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < den * 3 << BITRES {
                offset += nclogn >> 3;
            }
            ebits[j] = ((bits[j] + offset + (den << (BITRES - 1))) / (den << BITRES)).max(0);
            if c_count * ebits[j] > (bits[j] >> BITRES) {
                ebits[j] = bits[j] >> stereo >> BITRES;
            }
            ebits[j] = ebits[j].min(MAX_FINE_BITS);
            a.priority[j] = i32::from(ebits[j] * (den << BITRES) >= bits[j] + offset);
            bits[j] -= c_count * ebits[j] << BITRES;
        } else {
            excess = (bits[j] - (c_count << BITRES)).max(0);
            bits[j] -= excess;
            ebits[j] = 0;
            a.priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine = (excess >> (stereo + BITRES)).min(MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = extra_fine * c_count << BITRES;
            a.priority[j] = i32::from(extra_bits >= excess - balance);
            excess -= extra_bits;
        }
        balance = excess;
    }
    a.balance = balance;
    for j in coded..end {
        ebits[j] = bits[j] >> stereo >> BITRES;
        bits[j] = 0;
        a.priority[j] = i32::from(ebits[j] < 1);
    }
    coded as i32
}

fn compute_allocation(fr: &mut Frame, start: usize, end: usize, trim: i32, total: i32, c_count: i32, lm: i32, rc: &mut Rc<'_>) {
    let mut total = total.max(0);
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let (mut intensity_rsv, mut dual_stereo_rsv) = (0, 0);
    if c_count == 2 {
        intensity_rsv = i32::from(CELT_LOG2_FRAC[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    for j in start..end {
        let w = eb(j + 1) - eb(j);
        fr.thresh[j] = (c_count << BITRES).max((3 * w << lm << BITRES) >> 4);
        fr.trim_offset[j] = c_count * w * (trim - 5 - lm) * (end - j - 1) as i32 * (1 << (lm + BITRES)) >> 6;
        if w << lm == 1 {
            fr.trim_offset[j] -= c_count << BITRES;
        }
    }
    let (mut lo, mut hi) = (1i32, CELT_ALLOC_VECTORS as i32 - 1);
    loop {
        let mut done = false;
        let mut psum = 0;
        let mid = (lo + hi) >> 1;
        for j in (start..end).rev() {
            let w = eb(j + 1) - eb(j);
            let mut bitsj = c_count * w * i32::from(CELT_ALLOC[mid as usize][j]) << lm >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + fr.trim_offset[j]).max(0);
            }
            bitsj += fr.offsets[j];
            if bitsj >= fr.thresh[j] || done {
                done = true;
                psum += bitsj.min(fr.cap[j]);
            } else if bitsj >= c_count << BITRES {
                psum += c_count << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    hi = lo;
    lo -= 1;
    for j in start..end {
        let w = eb(j + 1) - eb(j);
        let mut b1 = c_count * w * i32::from(CELT_ALLOC[lo as usize][j]) << lm >> 2;
        let mut b2 = if hi >= CELT_ALLOC_VECTORS as i32 {
            fr.cap[j]
        } else {
            c_count * w * i32::from(CELT_ALLOC[hi as usize][j]) << lm >> 2
        };
        if b1 > 0 {
            b1 = (b1 + fr.trim_offset[j]).max(0);
        }
        if b2 > 0 {
            b2 = (b2 + fr.trim_offset[j]).max(0);
        }
        if lo > 0 {
            b1 += fr.offsets[j];
        }
        b2 += fr.offsets[j];
        if fr.offsets[j] > 0 {
            skip_start = j;
        }
        fr.bits1[j] = b1;
        fr.bits2[j] = (b2 - b1).max(0);
    }
    fr.alloc.coded_bands = interp_bits2pulses(
        start, end, skip_start, &fr.bits1, &fr.bits2, &fr.thresh, &fr.cap, total, skip_rsv, intensity_rsv,
        dual_stereo_rsv, c_count, lm, rc, &mut fr.alloc,
    );
}

// y = x + g * taps(y[-T]) in place, cross-fading from the old period, gain and
// taps to the new ones over the first CELT_OVERLAP samples. The tap weights are
// rounded to Q15 as the reference decoder rounds them, which the test vectors
// were made with. Each mulh() floors, and in this recursive filter that bias
// builds up audibly in quiet passages, so one LSB per term is added back.
#[allow(clippy::too_many_arguments)]
unsafe fn comb_filter(y: *mut i32, t0: i32, t1: i32, n: i32, g0: i32, g1: i32, tapset0: i32, tapset1: i32) {
    if g0 == 0 && g1 == 0 {
        return;
    }
    let gains = |g: i32, t: i32, i: usize| g * i32::from(CELT_COMB_GAINS[t as usize][i]) >> 15;
    let (g00, g01, g02) = (gains(g0, tapset0, 0), gains(g0, tapset0, 1), gains(g0, tapset0, 2));
    let (g10, g11, g12) = (gains(g1, tapset1, 0), gains(g1, tapset1, 1), gains(g1, tapset1, 2));
    let at = |i: i32| unsafe { *y.offset(i as isize) };
    let mut i = 0;
    while i < CELT_OVERLAP as i32 && i < n {
        let w = i32::from(CELT_WINDOW[i as usize]);
        let f = w * w >> 15;
        let a0 = ((32767 - f) * g00 >> 15) << 16;
        let a1 = ((32767 - f) * g01 >> 15) << 16;
        let a2 = ((32767 - f) * g02 >> 15) << 16;
        let b0 = (f * g10 >> 15) << 16;
        let b1 = (f * g11 >> 15) << 16;
        let b2 = (f * g12 >> 15) << 16;
        let acc = mulh(at(i - t0), a0)
            .wrapping_add(mulh(at(i - t0 - 1).wrapping_add(at(i - t0 + 1)), a1))
            .wrapping_add(mulh(at(i - t0 - 2).wrapping_add(at(i - t0 + 2)), a2))
            .wrapping_add(mulh(at(i - t1), b0))
            .wrapping_add(mulh(at(i - t1 - 1).wrapping_add(at(i - t1 + 1)), b1))
            .wrapping_add(mulh(at(i - t1 - 2).wrapping_add(at(i - t1 + 2)), b2));
        unsafe { *y.offset(i as isize) = sat_sig(i64::from(at(i)) + i64::from(acc.wrapping_mul(2)) + 6) };
        i += 1;
    }
    if g1 == 0 {
        return;
    }
    let (c0, c1, c2) = (g10 << 16, g11 << 16, g12 << 16);
    while i < n {
        let acc = mulh(at(i - t1), c0)
            .wrapping_add(mulh(at(i - t1 - 1).wrapping_add(at(i - t1 + 1)), c1))
            .wrapping_add(mulh(at(i - t1 - 2).wrapping_add(at(i - t1 + 2)), c2));
        unsafe { *y.offset(i as isize) = sat_sig(i64::from(at(i)) + i64::from(acc.wrapping_mul(2)) + 3) };
        i += 1;
    }
}

impl Celt {
    pub fn new(channels: usize, caps: Caps) -> Result<Self> {
        let mut mem = [None, None];
        for m in &mut mem[..channels] {
            *m = Some(heap::zeroed_slice(CELT_BUF + CELT_OVERLAP, caps)?);
        }
        let mut st = Self {
            channels,
            stream_channels: channels as u8,
            start: 0,
            end: CELT_BANDS as u8,
            stereo_inversion: false,
            rng: 0,
            loss_count: 0,
            pf_period: 0,
            pf_period_old: 0,
            pf_gain: 0,
            pf_gain_old: 0,
            pf_tapset: 0,
            pf_tapset_old: 0,
            preemph_mem: [0; 2],
            old_e: [0; 2 * CELT_BANDS],
            old_loge: [0; 2 * CELT_BANDS],
            old_loge2: [0; 2 * CELT_BANDS],
            background_loge: [0; 2 * CELT_BANDS],
            mem,
            freq: heap::zeroed_slice(2 * CELT_MAX_FRAME, caps)?,
            x: heap::zeroed_slice(2 * CELT_MAX_FRAME, caps)?,
            norm: heap::zeroed_slice(2 * 8 * 100, caps)?,
            scratch: heap::zeroed_slice(2 * 176, caps)?,
            iy: heap::zeroed_slice(MAX_PVQ_N, caps)?,
            pvq: Pvq::new(caps)?,
            mdct: Mdct::new(caps)?,
            frame: heap::zeroed_box(caps)?,
        };
        st.reset();
        Ok(st)
    }

    pub fn reset(&mut self) {
        for m in self.mem.iter_mut().flatten() {
            m.fill(0);
        }
        self.stream_channels = self.channels as u8;
        self.start = 0;
        self.end = CELT_BANDS as u8;
        self.rng = 0;
        self.loss_count = 0;
        self.pf_period = 0;
        self.pf_period_old = 0;
        self.pf_gain = 0;
        self.pf_gain_old = 0;
        self.pf_tapset = 0;
        self.pf_tapset_old = 0;
        self.preemph_mem = [0; 2];
        self.old_e = [0; 2 * CELT_BANDS];
        self.old_loge = [-db(28.0); 2 * CELT_BANDS];
        self.old_loge2 = [-db(28.0); 2 * CELT_BANDS];
        self.background_loge = [0; 2 * CELT_BANDS];
    }

    fn decode_coarse_energy(&mut self, rc: &mut Rc<'_>, intra: bool, c_count: usize, lm: usize) {
        let prob = &CELT_E_PROB[lm][usize::from(intra)];
        let mut prev = [0i32; 2];
        let coef = if intra { 0 } else { i32::from(CELT_PRED_COEF[lm]) };
        let beta = if intra { 4915 } else { i32::from(CELT_BETA_COEF[lm]) };
        let budget = rc.storage as i32 * 8;
        for i in usize::from(self.start)..usize::from(self.end) {
            for c in 0..c_count {
                let tell = rc.tell();
                let qi = if budget - tell >= 15 {
                    let pi = 2 * i.min(20);
                    laplace_decode(rc, u32::from(prob[pi]) << 7, i32::from(prob[pi + 1]) << 6)
                } else if budget - tell >= 2 {
                    let qi = rc.icdf(&CELT_SMALL_ENERGY_ICDF, 2);
                    (qi >> 1) ^ -(qi & 1)
                } else if budget - tell >= 1 {
                    -i32::from(rc.bit_logp(1))
                } else {
                    -1
                };
                let q = qi * 1024;
                let e = &mut self.old_e[c * CELT_BANDS + i];
                if *e < -db(9.0) {
                    *e = -db(9.0);
                }
                let mut tmp = ((coef * *e + 128) >> 8) + prev[c] + q * 128;
                if tmp < -db(28.0) * 128 {
                    tmp = -db(28.0) * 128;
                }
                *e = (tmp + 64) >> 7;
                prev[c] = prev[c] + q * 128 - beta * ((q + 128) >> 8);
            }
        }
    }

    fn decode_tf(&self, rc: &mut Rc<'_>, transient: bool, tf_res: &mut [i32; CELT_BANDS], lm: usize) {
        let mut budget = rc.storage * 8;
        let mut tell = rc.tell() as u32;
        let mut logp = if transient { 2 } else { 4 };
        let select_rsv = lm > 0 && tell + logp + 1 <= budget;
        budget -= u32::from(select_rsv);
        let (mut changed, mut curr) = (0usize, 0i32);
        let (start, end) = (usize::from(self.start), usize::from(self.end));
        for r in &mut tf_res[start..end] {
            if tell + logp <= budget {
                curr ^= i32::from(rc.bit_logp(logp));
                tell = rc.tell() as u32;
                changed |= curr as usize;
            }
            *r = curr;
            logp = if transient { 4 } else { 5 };
        }
        let t = 4 * usize::from(transient);
        let mut select = 0usize;
        if select_rsv && CELT_TF_SELECT[lm][t + changed] != CELT_TF_SELECT[lm][t + 2 + changed] {
            select = usize::from(rc.bit_logp(1));
        }
        for r in &mut tf_res[start..end] {
            *r = i32::from(CELT_TF_SELECT[lm][t + 2 * select + *r as usize]);
        }
    }

    fn decode_fine_energy(&mut self, rc: &mut Rc<'_>, fine: &[i32; CELT_BANDS], c_count: usize) {
        for i in usize::from(self.start)..usize::from(self.end) {
            if fine[i] <= 0 {
                continue;
            }
            for c in 0..c_count {
                let q2 = rc.bits(fine[i]) as i32;
                self.old_e[c * CELT_BANDS + i] += ((q2 * 1024 + 512) >> fine[i]) - 512;
            }
        }
    }

    fn decode_energy_finalise(&mut self, rc: &mut Rc<'_>, a: &Alloc, bits_left: i32, c_count: usize) {
        let mut bits_left = bits_left;
        for prio in 0..2 {
            let mut i = usize::from(self.start);
            while i < usize::from(self.end) && bits_left >= c_count as i32 {
                if a.fine[i] < MAX_FINE_BITS && a.priority[i] == prio {
                    for c in 0..c_count {
                        let q2 = rc.bits(1) as i32;
                        self.old_e[c * CELT_BANDS + i] += (q2 * 1024 - 512) >> (a.fine[i] + 1);
                        bits_left -= 1;
                    }
                }
                i += 1;
            }
        }
    }

    fn denormalise(&mut self, log_e: LogE, end: usize, c_count: usize, lm: i32, silence: bool) {
        let m = 1i32 << lm;
        let n = (m * 120) as usize;
        let scale = (1 << SIG_SHIFT) as f32 / NORM_ONE as f32;
        let sat = SIG_SAT as f32;
        for c in 0..c_count {
            let f = &mut self.freq[c * n..(c + 1) * n];
            f.fill(0);
            if silence {
                continue;
            }
            let log_e = match log_e {
                LogE::Old => &self.old_e,
                LogE::Background => &self.background_loge,
            };
            for i in usize::from(self.start)..end {
                let lg = (log_e[c * CELT_BANDS + i] + (i32::from(CELT_E_MEANS[i]) << 6)).min(db(32.0));
                let g = exp2(lg as f32 / 1024.0) * scale;
                let (lo, hi) = ((m * eb(i)) as usize, (m * eb(i + 1)) as usize);
                for (o, &s) in f[lo..hi].iter_mut().zip(&self.x[c * n + lo..c * n + hi]) {
                    let mut v = s as f32 * g;
                    if v > sat {
                        v = sat;
                    }
                    if v < -sat {
                        v = -sat;
                    }
                    *o = lrintf(v);
                }
            }
        }
    }

    fn synthesize(&mut self, log_e: LogE, c_count: usize, lm: i32, short_blocks: bool, end: usize, silence: bool) {
        let cc = self.channels;
        let n = 120usize << lm;
        self.denormalise(log_e, end, c_count, lm, silence);
        if cc == 2 && c_count == 1 {
            self.freq.copy_within(..n, n);
        } else if cc == 1 && c_count == 2 {
            for i in 0..n {
                self.freq[i] = ((i64::from(self.freq[i]) + i64::from(self.freq[n + i])) >> 1) as i32;
            }
        }
        for c in 0..cc {
            let mem = self.mem[c].as_mut().expect("channel");
            mem.copy_within(n..CELT_BUF, 0);
            mem.copy_within(CELT_BUF..CELT_BUF + CELT_OVERLAP, CELT_BUF - n);
            let out = mem[CELT_BUF - n..].as_mut_ptr();
            let freq = self.freq[c * n..].as_ptr();
            unsafe {
                if short_blocks {
                    let blocks = 1usize << lm;
                    for b in 0..blocks {
                        self.mdct.imdct(freq.add(b), blocks, 3, out.add(120 * b));
                    }
                } else {
                    self.mdct.imdct(freq, 1, 3 - lm as usize, out);
                }
            }
        }
    }

    unsafe fn deemphasis(&mut self, n: usize, pcm: *mut i32) {
        let cc = self.channels;
        for c in 0..cc {
            let x = &self.mem[c].as_ref().expect("channel")[CELT_BUF - n..CELT_BUF];
            let mut m = self.preemph_mem[c];
            for (j, &v) in x.iter().enumerate() {
                let t = sat_sig(i64::from(v) + i64::from(m));
                // The floor's bias, as in comb_filter().
                m = mulh(t, PREEMPH_Q15 << 16).wrapping_mul(2).wrapping_add(1);
                unsafe { *pcm.add(j * cc + c) = t };
            }
            self.preemph_mem[c] = m;
        }
    }

    unsafe fn decode_lost(&mut self, lm: i32, pcm: *mut i32) {
        let n = 120usize << lm;
        let c_count = self.channels;
        let m = 1i32 << lm;
        let (start, end) = (usize::from(self.start), usize::from(self.end));
        if self.loss_count < 5 {
            let decay = if self.loss_count == 0 { db(1.5) } else { db(0.5) };
            for c in 0..c_count {
                for i in start..end {
                    self.old_e[c * CELT_BANDS + i] -= decay;
                }
            }
        }
        let mut seed = self.rng;
        for c in 0..c_count {
            let x = &mut self.x[c * n..(c + 1) * n];
            x.fill(0);
            for i in start..end {
                let off = (m * eb(i)) as usize;
                let len = (m * (eb(i + 1) - eb(i))) as usize;
                for v in &mut x[off..off + len] {
                    seed = lcg(seed);
                    *v = (seed as i32) >> 12;
                }
                unsafe { renormalise(x[off..].as_mut_ptr(), len, 1.0) };
            }
        }
        self.rng = seed;
        let log_e = if self.loss_count >= 5 { LogE::Background } else { LogE::Old };
        self.synthesize(log_e, c_count, lm, false, end, false);
        unsafe { self.deemphasis(n, pcm) };
        self.loss_count += 1;
    }

    /// Decodes `n = 120 << lm` samples per channel as PCM x 2^SIG_SHIFT into
    /// `pcm`, interleaved; `rc` `None` conceals a lost frame.
    pub unsafe fn decode(&mut self, rc: Option<&mut Rc<'_>>, len: u32, lm: i32, pcm: *mut i32) {
        let n = 120usize << lm;
        let m = 1i32 << lm;
        let cc = self.channels;
        let c_count = usize::from(self.stream_channels);
        let Some(rc) = rc.filter(|_| len > 1) else {
            unsafe { self.decode_lost(lm, pcm) };
            return;
        };
        let (start, end) = (usize::from(self.start), usize::from(self.end));
        let lmu = lm as usize;
        let cc32 = c_count as i32;

        if c_count == 1 {
            for i in 0..CELT_BANDS {
                self.old_e[i] = self.old_e[i].max(self.old_e[CELT_BANDS + i]);
            }
        }

        let len_i = len as i32;
        let mut total_bits = len_i * 8;
        let mut tell = rc.tell();
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            rc.bit_logp(15)
        } else {
            false
        };
        if silence {
            tell = len_i * 8;
            rc.nbits_total += tell - rc.tell();
        }

        let (mut pf_gain, mut pf_pitch, mut pf_tapset) = (0, 0, 0);
        if start == 0 && tell + 16 <= total_bits {
            if rc.bit_logp(1) {
                let octave = rc.uint(6) as i32;
                pf_pitch = (16 << octave) + rc.bits(4 + octave) as i32 - 1;
                let qg = rc.bits(3) as i32;
                if rc.tell() + 2 <= total_bits {
                    pf_tapset = rc.icdf(&CELT_TAPSET_ICDF, 2);
                }
                pf_gain = 3072 * (qg + 1);
            }
            tell = rc.tell();
        }

        let mut transient = false;
        if lm > 0 && tell + 3 <= total_bits {
            transient = rc.bit_logp(3);
            tell = rc.tell();
        }
        let intra = tell + 3 <= total_bits && rc.bit_logp(3);
        self.decode_coarse_energy(rc, intra, c_count, lmu);

        let fr = unsafe { &mut *ptr::from_mut(&mut *self.frame) };
        fr.tf_res = [0; CELT_BANDS];
        self.decode_tf(rc, transient, &mut fr.tf_res, lmu);

        tell = rc.tell();
        let mut spread = SPREAD_NORMAL;
        if tell + 4 <= total_bits {
            spread = rc.icdf(&CELT_SPREAD_ICDF, 5);
        }

        for i in 0..CELT_BANDS {
            let w = (eb(i + 1) - eb(i)) << lm;
            let caps = i32::from(CELT_CACHE_CAPS[CELT_BANDS * (2 * lmu + c_count - 1) + i]);
            fr.cap[i] = (caps + 64) * cc32 * w >> 2;
        }

        fr.offsets = [0; CELT_BANDS];
        let mut dynalloc_logp = 6;
        total_bits <<= BITRES;
        tell = rc.tell_frac() as i32;
        for i in start..end {
            let width = cc32 * (eb(i + 1) - eb(i)) << lm;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            while tell + (loop_logp << BITRES) < total_bits && boost < fr.cap[i] {
                let flag = rc.bit_logp(loop_logp as u32);
                tell = rc.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits -= quanta;
                loop_logp = 1;
            }
            fr.offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = (dynalloc_logp - 1).max(2);
            }
        }

        let trim = if tell + (6 << BITRES) <= total_bits { rc.icdf(&CELT_TRIM_ICDF, 7) } else { 5 };

        let mut bits = ((len_i * 8) << BITRES) - rc.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if transient && lm >= 2 && bits >= (lm + 2) << BITRES { 1 << BITRES } else { 0 };
        bits -= anti_collapse_rsv;
        compute_allocation(fr, start, end, trim, bits, cc32, lm, rc);
        let alloc = &fr.alloc;

        self.decode_fine_energy(rc, &alloc.fine, c_count);

        let x = self.x.as_mut_ptr();
        self.x[..c_count * n].fill(0);
        let scratch = self.scratch.as_mut_ptr();
        let mut ctx = Bands {
            rc: &mut *rc,
            pvq: &self.pvq,
            lm,
            spread,
            intensity: alloc.intensity,
            stereo_inversion: self.stereo_inversion,
            remaining_bits: 0,
            seed: self.rng,
            scratch,
            lowband_scratch: unsafe { scratch.add(176) },
            iy: self.iy.as_mut_ptr(),
            coded_bands: alloc.coded_bands,
        };
        let y = if c_count == 2 { unsafe { x.add(n) } } else { ptr::null_mut() };
        unsafe {
            ctx.quant_all_bands(
                start,
                end,
                x,
                y,
                &mut fr.collapse,
                &alloc.pulses,
                if transient { m } else { 0 },
                alloc.dual_stereo,
                &fr.tf_res,
                len_i * (8 << BITRES) - anti_collapse_rsv,
                alloc.balance,
                self.norm.as_mut_ptr(),
            );
        }
        let seed = ctx.seed;

        let anti_collapse_on = anti_collapse_rsv > 0 && rc.bits(1) != 0;
        let left = len_i * 8 - rc.tell();
        self.decode_energy_finalise(rc, alloc, left, c_count);
        if anti_collapse_on {
            unsafe {
                anti_collapse(x, &fr.collapse, lm, c_count, n, start, end, &self.old_e, &self.old_loge, &self.old_loge2, &alloc.pulses, seed);
            }
        }

        if silence {
            self.old_e[..c_count * CELT_BANDS].fill(-db(28.0));
        }
        self.synthesize(LogE::Old, c_count, lm, transient, end, silence);

        for c in 0..cc {
            let out = self.mem[c].as_mut().expect("channel")[CELT_BUF - n..].as_mut_ptr();
            self.pf_period = self.pf_period.max(COMB_MIN_PERIOD);
            self.pf_period_old = self.pf_period_old.max(COMB_MIN_PERIOD);
            unsafe {
                comb_filter(out, self.pf_period_old, self.pf_period, 120, self.pf_gain_old, self.pf_gain, self.pf_tapset_old, self.pf_tapset);
                if lm != 0 {
                    comb_filter(out.add(120), self.pf_period, pf_pitch, n as i32 - 120, self.pf_gain, pf_gain, self.pf_tapset, pf_tapset);
                }
            }
        }
        self.pf_period_old = self.pf_period;
        self.pf_gain_old = self.pf_gain;
        self.pf_tapset_old = self.pf_tapset;
        self.pf_period = pf_pitch;
        self.pf_gain = pf_gain;
        self.pf_tapset = pf_tapset;
        if lm != 0 {
            self.pf_period_old = self.pf_period;
            self.pf_gain_old = self.pf_gain;
            self.pf_tapset_old = self.pf_tapset;
        }

        if c_count == 1 {
            self.old_e.copy_within(..CELT_BANDS, CELT_BANDS);
        }
        if !transient {
            self.old_loge2 = self.old_loge;
            self.old_loge = self.old_e;
            for i in 0..2 * CELT_BANDS {
                self.background_loge[i] = (self.background_loge[i] + m).min(self.old_e[i]);
            }
        } else {
            for i in 0..2 * CELT_BANDS {
                self.old_loge[i] = self.old_loge[i].min(self.old_e[i]);
            }
        }
        for c in 0..2 {
            for i in 0..CELT_BANDS {
                if i >= start && i < end {
                    continue;
                }
                self.old_e[c * CELT_BANDS + i] = 0;
                self.old_loge[c * CELT_BANDS + i] = -db(28.0);
                self.old_loge2[c * CELT_BANDS + i] = -db(28.0);
            }
        }
        self.rng = rc.rng;
        unsafe { self.deemphasis(n, pcm) };
        self.loss_count = 0;
    }
}

#[derive(Clone, Copy)]
enum LogE {
    Old,
    Background,
}
