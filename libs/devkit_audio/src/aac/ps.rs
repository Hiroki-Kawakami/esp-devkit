/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::math::lrintf;
use devkit::{A16, Zeroable};

use super::kernels::{ps_allpass, ps_gain, ps_mix, ps_mix_real, ps_unzip, ps_zip};
use super::sbr_hf::to_fixed;
use super::tables::*;
use super::{HUFF_BASE_PS, huff};
use crate::bits::Bits;

const PS_H_IID_DF1: usize = HUFF_BASE_PS;
const PS_H_IID_DT1: usize = HUFF_BASE_PS + 1;
const PS_H_IID_DF0: usize = HUFF_BASE_PS + 2;
const PS_H_IID_DT0: usize = HUFF_BASE_PS + 3;
const PS_H_ICC_DF: usize = HUFF_BASE_PS + 4;
const PS_H_ICC_DT: usize = HUFF_BASE_PS + 5;
const PS_H_IPD_DF: usize = HUFF_BASE_PS + 6;
const PS_H_IPD_DT: usize = HUFF_BASE_PS + 7;
const PS_H_OPD_DF: usize = HUFF_BASE_PS + 8;
const PS_H_OPD_DT: usize = HUFF_BASE_PS + 9;

const SLOTS: i32 = 32;
const BANDS: usize = 71;
const PAR: usize = 20;
const AP_BANDS: usize = 30;
const SHORT: usize = 42;
const DELAY: usize = 14;
const MAX_ENV: usize = 5;
const MAX_PAR: usize = 34;
const IPDOPD: usize = 11;
const LANES: usize = 72;
const GROUPS: usize = LANES / 4;
const D1_GROUP: usize = 11;

type Par = [[i8; MAX_PAR]; MAX_ENV];

#[repr(C)]
pub(super) struct Ps {
    l: A16<[i32; 2 * LANES]>,
    r: A16<[i32; 2 * LANES]>,
    d2: A16<[[i32; 64]; 2]>,
    ap: A16<[[[i32; 64]; 8]; 3]>,
    gain: A16<[i16; GROUPS * 8]>,
    mix_acc: A16<[i32; GROUPS * 32]>,
    mix_step: A16<[i32; GROUPS * 32]>,
    d14: [[[i32; SHORT - AP_BANDS]; 2]; DELAY],
    d1: A16<[i32; 8 * (GROUPS - D1_GROUP) + 2]>,
    start: bool,
    enable_iid: bool,
    enable_icc: bool,
    enable_ext: bool,
    enable_ipdopd: bool,
    iid_quant: u8,
    nr_iid: u8,
    nr_icc: u8,
    nr_ipdopd: u8,
    icc_mode: u8,
    num_env: u8,
    num_env_old: u8,
    border: [i8; MAX_ENV + 1],
    iid: Par,
    icc: Par,
    ipd: Par,
    opd: Par,
    h: [[[f32; PAR]; MAX_ENV + 1]; 4],
    hi: [[[f32; PAR]; MAX_ENV + 1]; 4],
    ipd_hist: [u8; IPDOPD],
    opd_hist: [u8; IPDOPD],
    in_buf: [[[f32; 2]; 44]; 3],
    pos14: u8,
    pos_ap: u8,
    env: i8,
    peak_decay_nrg: [f32; PAR],
    power_smooth: [f32; PAR],
    peak_decay_diff_smooth: [f32; PAR],
}

unsafe impl Zeroable for Ps {}

const NUM_ENV: [[u8; 4]; 2] = [[0, 1, 2, 4], [1, 2, 3, 4]];
const NR_IIDICC: [u8; 6] = [10, 20, 34, 10, 20, 34];
const NR_IPDOPD: [u8; 6] = [5, 11, 17, 5, 11, 17];

#[allow(clippy::too_many_arguments)]
fn read_par(b: &mut Bits<'_>, par: &mut Par, table: usize, e: usize, dt: bool, num: usize, num_env_old: u8, limit: i32, mask: i32) -> bool {
    let mut v = 0i32;
    let prev = if e != 0 { e as i32 - 1 } else { i32::from(num_env_old) - 1 }.max(0) as usize;
    for i in 0..num {
        v = if dt { i32::from(par[prev][i]) + huff(b, table) } else { v + huff(b, table) };
        if mask != 0 {
            v &= mask;
        }
        if limit >= 0 && (v > limit || v < -limit) {
            return false;
        }
        par[e][i] = v as i8;
    }
    true
}

impl Ps {
    fn read_extension(&mut self, b: &mut Bits<'_>, id: u32) -> i32 {
        let start = b.pos;
        if id != 0 {
            return 0;
        }
        self.enable_ipdopd = b.bit();
        if self.enable_ipdopd {
            let n = usize::from(self.nr_ipdopd);
            for e in 0..usize::from(self.num_env) {
                let dt = b.bit();
                read_par(b, &mut self.ipd, if dt { PS_H_IPD_DT } else { PS_H_IPD_DF }, e, dt, n, self.num_env_old, -1, 7);
                let dt = b.bit();
                read_par(b, &mut self.opd, if dt { PS_H_OPD_DT } else { PS_H_OPD_DF }, e, dt, n, self.num_env_old, -1, 7);
            }
        }
        b.bit();
        (b.pos - start) as i32
    }

    fn read_data(&mut self, b: &mut Bits<'_>, header: &mut bool) -> bool {
        *header = b.bit();
        if *header {
            self.enable_iid = b.bit();
            if self.enable_iid {
                let mode = b.get(3) as usize;
                if mode > 5 {
                    return false;
                }
                self.nr_iid = NR_IIDICC[mode];
                self.iid_quant = u8::from(mode > 2);
                self.nr_ipdopd = NR_IPDOPD[mode];
            }
            self.enable_icc = b.bit();
            if self.enable_icc {
                self.icc_mode = b.get(3) as u8;
                if self.icc_mode > 5 {
                    return false;
                }
                self.nr_icc = NR_IIDICC[usize::from(self.icc_mode)];
            }
            self.enable_ext = b.bit();
        }
        let frame_class = b.get(1) as usize;
        self.num_env_old = self.num_env;
        self.num_env = NUM_ENV[frame_class][b.get(2) as usize];
        let num_env = usize::from(self.num_env);
        self.border[0] = -1;
        if frame_class != 0 {
            for e in 1..=num_env {
                self.border[e] = b.get(5) as i8;
                if self.border[e] < self.border[e - 1] {
                    return false;
                }
            }
        } else {
            let shift = match self.num_env {
                4 => 2,
                2 => 1,
                _ => 0,
            };
            for e in 1..=num_env {
                self.border[e] = ((e as i32 * SLOTS >> shift) - 1) as i8;
            }
        }
        if self.enable_iid {
            for e in 0..num_env {
                let dt = b.bit();
                const TABLES: [usize; 4] = [PS_H_IID_DF0, PS_H_IID_DF1, PS_H_IID_DT0, PS_H_IID_DT1];
                let table = TABLES[2 * usize::from(dt) + usize::from(self.iid_quant)];
                let limit = 7 + 8 * i32::from(self.iid_quant);
                if !read_par(b, &mut self.iid, table, e, dt, usize::from(self.nr_iid), self.num_env_old, limit, 0) {
                    return false;
                }
            }
        } else {
            self.iid = [[0; MAX_PAR]; MAX_ENV];
        }
        if self.enable_icc {
            for e in 0..num_env {
                let dt = b.bit();
                let table = if dt { PS_H_ICC_DT } else { PS_H_ICC_DF };
                if !read_par(b, &mut self.icc, table, e, dt, usize::from(self.nr_icc), self.num_env_old, 7, 0) {
                    return false;
                }
                if self.icc[e][..usize::from(self.nr_icc)].iter().any(|&v| v < 0) {
                    return false;
                }
            }
        } else {
            self.icc = [[0; MAX_PAR]; MAX_ENV];
        }
        if self.enable_ext {
            let mut cnt = b.get(4) as i32;
            if cnt == 15 {
                cnt += b.get(8) as i32;
            }
            cnt *= 8;
            while cnt > 7 {
                let id = b.get(2);
                cnt -= 2 + self.read_extension(b, id);
            }
            if cnt < 0 {
                return false;
            }
            b.pos += cnt as u32;
        }
        if self.num_env == 0 || i32::from(self.border[num_env]) < SLOTS - 1 {
            let source = if num_env != 0 { num_env as i32 - 1 } else { i32::from(self.num_env_old) - 1 };
            if source >= 0 && source as usize != num_env {
                let source = source as usize;
                if self.enable_iid {
                    self.iid[num_env] = self.iid[source];
                }
                if self.enable_icc {
                    self.icc[num_env] = self.icc[source];
                }
                if self.enable_ipdopd {
                    self.ipd[num_env] = self.ipd[source];
                    self.opd[num_env] = self.opd[source];
                }
            }
            self.num_env += 1;
            self.border[usize::from(self.num_env)] = (SLOTS - 1) as i8;
        }
        if !self.enable_ipdopd {
            self.ipd = [[0; MAX_PAR]; MAX_ENV];
            self.opd = [[0; MAX_PAR]; MAX_ENV];
        }
        true
    }

    pub(super) fn parse(&mut self, b: &mut Bits<'_>, bits: i32) -> i32 {
        let start = b.pos;
        let mut header = false;
        if self.read_data(b, &mut header) && (b.pos - start) as i32 <= bits {
            if header {
                self.start = true;
            }
            return (b.pos - start) as i32;
        }
        self.start = false;
        self.iid = [[0; MAX_PAR]; MAX_ENV];
        self.icc = [[0; MAX_PAR]; MAX_ENV];
        b.pos = start + bits as u32;
        bits
    }

    pub(super) fn ready(&self) -> bool {
        self.start
    }
}

fn hybrid8(out: &mut [[f32; 2]], input: &[[f32; 2]]) {
    let mut t = [[0f32; 2]; 8];
    for q in 0..8 {
        let f = &PS_F20[q];
        let mut re = f[6][0] * input[6][0];
        let mut im = f[6][0] * input[6][1];
        for j in 0..6 {
            re += f[j][0] * (input[j][0] + input[12 - j][0]) - f[j][1] * (input[j][1] - input[12 - j][1]);
            im += f[j][0] * (input[j][1] + input[12 - j][1]) + f[j][1] * (input[j][0] - input[12 - j][0]);
        }
        t[q] = [re, im];
    }
    out[0] = t[6];
    out[1] = t[7];
    out[2] = t[0];
    out[3] = t[1];
    out[4] = [t[2][0] + t[5][0], t[2][1] + t[5][1]];
    out[5] = [t[3][0] + t[4][0], t[3][1] + t[4][1]];
}

fn hybrid2(out: &mut [[f32; 2]], input: &[[f32; 2]], reverse: usize) {
    const G: [f32; 7] = [0.0, 0.01899487526049, 0.0, -0.07293139167538, 0.0, 0.30596630545168, 0.5];
    let re_in = G[6] * input[6][0];
    let im_in = G[6] * input[6][1];
    let (mut re_op, mut im_op) = (0f32, 0f32);
    for j in (0..6).step_by(2) {
        re_op += G[j + 1] * (input[j + 1][0] + input[12 - j - 1][0]);
        im_op += G[j + 1] * (input[j + 1][1] + input[12 - j - 1][1]);
    }
    out[reverse] = [re_in + re_op, im_in + im_op];
    out[1 - reverse] = [re_in - re_op, im_in - im_op];
}

// Lane 10 stays empty so that QMF bands 4-63 fill whole groups.
#[inline(always)]
fn lane(k: usize, c: usize) -> usize {
    let p = if k < 10 { k } else { k + 1 };
    (p >> 2) * 8 + c * 4 + (p & 3)
}

fn map_to_20(dst: &mut [i8; PAR], par: &[i8; MAX_PAR], num: u8) {
    let p = |i: usize| i32::from(par[i]);
    match num {
        10 => {
            for b in 0..10 {
                dst[2 * b] = par[b];
                dst[2 * b + 1] = par[b];
            }
        }
        5 => {
            dst[10] = 0;
            for b in 0..5 {
                dst[2 * b] = par[b];
                dst[2 * b + 1] = par[b];
            }
        }
        34 | 17 => {
            dst[0] = ((2 * p(0) + p(1)) / 3) as i8;
            dst[1] = ((p(1) + 2 * p(2)) / 3) as i8;
            dst[2] = ((2 * p(3) + p(4)) / 3) as i8;
            dst[3] = ((p(4) + 2 * p(5)) / 3) as i8;
            dst[4] = ((p(6) + p(7)) / 2) as i8;
            dst[5] = ((p(8) + p(9)) / 2) as i8;
            dst[6] = par[10];
            dst[7] = par[11];
            dst[8] = ((p(12) + p(13)) / 2) as i8;
            dst[9] = ((p(14) + p(15)) / 2) as i8;
            dst[10] = par[16];
            dst[11] = par[17];
            dst[12] = par[18];
            dst[13] = par[19];
            dst[14] = ((p(20) + p(21)) / 2) as i8;
            dst[15] = ((p(22) + p(23)) / 2) as i8;
            dst[16] = ((p(24) + p(25)) / 2) as i8;
            dst[17] = ((p(26) + p(27)) / 2) as i8;
            dst[18] = ((p(28) + p(29) + p(30) + p(31)) / 4) as i8;
            dst[19] = ((p(32) + p(33)) / 2) as i8;
        }
        _ => dst.copy_from_slice(&par[..PAR]),
    }
}

#[inline(always)]
fn q30(v: f32) -> i32 {
    let v = v * 1073741824.0;
    if v > 2147483520.0 {
        2147483520
    } else if v < -2147483648.0 {
        i32::MIN
    } else {
        lrintf(v)
    }
}

unsafe fn synthesis(input: &[i32; 2 * LANES], dst: *mut i32) {
    let (mut re, mut im) = (0i32, 0i32);
    for k in 0..6 {
        re = re.wrapping_add(input[lane(k, 0)]);
        im = im.wrapping_add(input[lane(k, 1)]);
    }
    unsafe {
        *dst = re;
        *dst.add(1) = im;
        *dst.add(2) = input[lane(6, 0)].wrapping_add(input[lane(7, 0)]);
        *dst.add(3) = input[lane(6, 1)].wrapping_add(input[lane(7, 1)]);
        *dst.add(4) = input[lane(8, 0)].wrapping_add(input[lane(9, 0)]);
        *dst.add(5) = input[lane(8, 1)].wrapping_add(input[lane(9, 1)]);
        *dst.add(6) = input[lane(10, 0)];
        *dst.add(7) = input[lane(10, 1)];
        ps_zip(dst.add(8), input.as_ptr().add(24), (GROUPS - 3) as i32);
    }
}

impl Ps {
    fn analysis(&mut self, row: *const i32, n: usize) {
        let mut hy = [[0f32; 2]; 10];
        hybrid8(&mut hy, &self.in_buf[0][n..]);
        hybrid2(&mut hy[6..], &self.in_buf[1][n..], 1);
        hybrid2(&mut hy[8..], &self.in_buf[2][n..], 0);
        for (k, v) in hy.iter().enumerate() {
            self.l[lane(k, 0)] = to_fixed(v[0]);
            self.l[lane(k, 1)] = to_fixed(v[1]);
        }
        unsafe {
            self.l[lane(10, 0)] = *row.add(6);
            self.l[lane(10, 1)] = *row.add(7);
            ps_unzip(self.l.as_mut_ptr().add(24), row.add(8), (GROUPS - 3) as i32);
        }
    }

    fn decorrelate(&mut self, n: usize) {
        const FIRST: [u8; PAR - 1] = [4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17, 19, 22, 26, 31, 43, 72];
        let mut nrg = [0f32; LANES];
        let mut power = [0f32; PAR];
        for g in 0..GROUPS {
            for k in 0..4 {
                let re = self.l[8 * g + k] as f32;
                let im = self.l[8 * g + 4 + k] as f32;
                nrg[4 * g + k] = re * re + im * im;
            }
        }
        power[0] = nrg[1] + nrg[2];
        power[1] = nrg[0] + nrg[3];
        for i in 2..PAR {
            let mut sum = 0f32;
            for p in usize::from(FIRST[i - 2])..usize::from(FIRST[i - 1]) {
                sum += nrg[p];
            }
            power[i] = sum;
        }
        for i in 0..PAR {
            let decayed = 0.76592833836465 * self.peak_decay_nrg[i];
            self.peak_decay_nrg[i] = if decayed > power[i] { decayed } else { power[i] };
            self.power_smooth[i] += 0.25 * (power[i] - self.power_smooth[i]);
            self.peak_decay_diff_smooth[i] += 0.25 * (self.peak_decay_nrg[i] - power[i] - self.peak_decay_diff_smooth[i]);
            let denom = 1.5 * self.peak_decay_diff_smooth[i];
            let g = if denom > self.power_smooth[i] { self.power_smooth[i] / denom } else { 1.0 };
            let q = lrintf(g * 32767.0) as i16;
            if i < 2 {
                let (a, b) = if i != 0 { (0, 3) } else { (1, 2) };
                self.gain[a] = q;
                self.gain[4 + a] = q;
                self.gain[b] = q;
                self.gain[4 + b] = q;
                continue;
            }
            for p in usize::from(FIRST[i - 2])..usize::from(FIRST[i - 1]) {
                let at = 8 * (p >> 2) + (p & 3);
                self.gain[at] = q;
                self.gain[at + 4] = q;
            }
        }
        unsafe {
            ps_allpass(self.r.as_mut_ptr(), self.d2[n & 1].as_ptr(), self.ap.as_mut_ptr().cast(), i32::from(self.pos_ap));
        }
        let (l, d2) = (&self.l[..64], &mut self.d2[n & 1]);
        d2.copy_from_slice(l);
        self.pos_ap = (self.pos_ap + 1) & 7;
        let p = usize::from(self.pos14);
        for c in 0..2 {
            for k in AP_BANDS..SHORT {
                let at = lane(k, c);
                self.r[at] = self.d14[p][c][k - AP_BANDS];
                self.d14[p][c][k - AP_BANDS] = self.l[at];
            }
            let last = 8 * (GROUPS - D1_GROUP) + c;
            self.r[lane(SHORT, c)] = self.d1[last];
            self.d1[last] = self.l[lane(SHORT, c)];
        }
        let words = 8 * (GROUPS - D1_GROUP);
        self.r[8 * D1_GROUP..8 * D1_GROUP + words].copy_from_slice(&self.d1[..words]);
        self.d1[..words].copy_from_slice(&self.l[8 * D1_GROUP..8 * D1_GROUP + words]);
        self.pos14 = if p == DELAY - 1 { 0 } else { p as u8 + 1 };
        unsafe { ps_gain(self.r.as_mut_ptr(), self.gain.as_ptr(), GROUPS as i32) };
    }

    fn mixing_matrices(&mut self) {
        let lut = if self.icc_mode < 3 { &PS_HA } else { &PS_HB };
        if self.num_env_old != 0 {
            let old = usize::from(self.num_env_old);
            for j in 0..4 {
                self.h[j][0] = self.h[j][old];
                self.hi[j][0] = self.hi[j][old];
            }
        }
        let ipdopd = self.enable_ipdopd;
        for e in 0..usize::from(self.num_env) {
            let (mut iid, mut icc, mut ipd, mut opd) = ([0i8; PAR], [0i8; PAR], [0i8; PAR], [0i8; PAR]);
            map_to_20(&mut iid, &self.iid[e], self.nr_iid);
            map_to_20(&mut icc, &self.icc[e], self.nr_icc);
            if ipdopd {
                map_to_20(&mut ipd, &self.ipd[e], self.nr_ipdopd);
                map_to_20(&mut opd, &self.opd[e], self.nr_ipdopd);
            }
            for b in 0..PAR {
                let row = i32::from(iid[b]) + 7 + 23 * i32::from(self.iid_quant);
                let h = &lut[row as usize][icc[b] as usize];
                let [mut h11, mut h12, mut h21, mut h22] = *h;
                if ipdopd && b < IPDOPD {
                    let opd_idx = usize::from(self.opd_hist[b]) * 8 + opd[b] as usize;
                    let ipd_idx = usize::from(self.ipd_hist[b]) * 8 + ipd[b] as usize;
                    let [opd_re, opd_im] = PS_PD_SMOOTH[opd_idx];
                    let [ipd_re, ipd_im] = PS_PD_SMOOTH[ipd_idx];
                    self.opd_hist[b] = (opd_idx & 0x3f) as u8;
                    self.ipd_hist[b] = (ipd_idx & 0x3f) as u8;
                    let adj_re = opd_re * ipd_re + opd_im * ipd_im;
                    let adj_im = opd_im * ipd_re - opd_re * ipd_im;
                    self.hi[0][e + 1][b] = h11 * opd_im;
                    self.hi[1][e + 1][b] = h12 * adj_im;
                    self.hi[2][e + 1][b] = h21 * opd_im;
                    self.hi[3][e + 1][b] = h22 * adj_im;
                    h11 *= opd_re;
                    h12 *= adj_re;
                    h21 *= opd_re;
                    h22 *= adj_re;
                }
                self.h[0][e + 1][b] = h11;
                self.h[1][e + 1][b] = h12;
                self.h[2][e + 1][b] = h21;
                self.h[3][e + 1][b] = h22;
            }
        }
    }

    fn mix_start(&mut self, e: usize) {
        const ORDER: [usize; 4] = [0, 2, 1, 3];
        let width = 1.0 / (i32::from(self.border[e + 1]) - i32::from(self.border[e])) as f32;
        for k in 0..BANDS {
            let b = PS_K_TO_I[k] as usize;
            let p = if k < 10 { k } else { k + 1 };
            let at = 32 * (p >> 2) + (p & 3);
            for j in 0..4 {
                let o = ORDER[j];
                let (h0, h1) = (self.h[o][e][b], self.h[o][e + 1][b]);
                self.mix_acc[at + 8 * j] = q30(h0);
                self.mix_step[at + 8 * j] = q30((h1 - h0) * width);
                let (mut g0, mut g1) = (0f32, 0f32);
                if self.enable_ipdopd {
                    g0 = if k <= 1 { -self.hi[o][e][b] } else { self.hi[o][e][b] };
                    g1 = self.hi[o][e + 1][b];
                }
                self.mix_acc[at + 8 * j + 4] = q30(g0);
                self.mix_step[at + 8 * j + 4] = q30((g1 - g0) * width);
            }
        }
    }

    pub(super) fn begin(&mut self, x_low: *const i32, top: usize) {
        let top = top + BANDS - 64;
        for k in top..BANDS {
            for c in 0..2 {
                if k < AP_BANDS {
                    let at = lane(k, c);
                    self.d2[0][at] = 0;
                    self.d2[1][at] = 0;
                    for m in 0..3 {
                        for i in 0..8 {
                            self.ap[m][i][at] = 0;
                        }
                    }
                } else if k < SHORT {
                    for i in 0..DELAY {
                        self.d14[i][c][k - AP_BANDS] = 0;
                    }
                } else if k == SHORT {
                    self.d1[8 * (GROUPS - D1_GROUP) + c] = 0;
                } else {
                    self.d1[lane(k, c) - 8 * D1_GROUP] = 0;
                }
            }
        }
        for i in 0..3 {
            for j in 0..38 {
                unsafe {
                    self.in_buf[i][j + 6][0] = *x_low.add(j * 64 + 2 * i) as f32;
                    self.in_buf[i][j + 6][1] = *x_low.add(j * 64 + 2 * i + 1) as f32;
                }
            }
        }
        self.mixing_matrices();
        let num_env = usize::from(self.num_env);
        let mut e = 0;
        while e < num_env && self.border[e + 1] < 0 {
            e += 1;
        }
        if e < num_env {
            self.mix_start(e);
        }
        self.env = e as i8;
    }

    pub(super) fn slots(&mut self, l: *mut i32, r: *mut i32, n0: usize, count: usize) {
        let mut e = self.env as usize;
        let num_env = usize::from(self.num_env);
        for i in 0..count {
            let n = n0 + i;
            let (l, r) = unsafe { (l.add(i * 128), r.add(i * 128)) };
            self.analysis(l, n);
            self.decorrelate(n);
            while e < num_env && n as i32 > i32::from(self.border[e + 1]) {
                e += 1;
                while e < num_env && self.border[e + 1] == self.border[e] {
                    e += 1;
                }
                if e < num_env {
                    self.mix_start(e);
                }
            }
            let groups = GROUPS as i32;
            let (lp, rp) = (self.l.as_mut_ptr(), self.r.as_mut_ptr());
            let (acc, step) = (self.mix_acc.as_mut_ptr(), self.mix_step.as_ptr());
            unsafe {
                if e < num_env && self.enable_ipdopd {
                    ps_mix(lp, rp, acc, step, groups);
                } else if e < num_env {
                    ps_mix_real(lp, rp, acc, step, groups);
                }
                synthesis(&self.l, l);
                synthesis(&self.r, r);
            }
        }
        self.env = e as i8;
    }

    pub(super) fn end(&mut self) {
        for b in &mut self.in_buf {
            b.copy_within(32..38, 0);
        }
    }
}
