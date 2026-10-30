/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::A16;

use super::kernels::{self, split};
use super::tables::*;
use super::{FRAC, Frame, MAX_BACK, MODE_JOINT, PAD, State, headroom, mulh};
use crate::bits::Bits;
use crate::{Error, Result};

const XR_MAX: i32 = 1 << 27;
const Z_MAX: i32 = 1 << 29;

#[derive(Clone, Copy, Default)]
struct Granule {
    part23: u16,
    big_values: u16,
    sfc: u16,
    global_gain: u8,
    block_type: u8,
    mixed: bool,
    table: [u8; 3],
    sbg: [u8; 3],
    region0: u8,
    region1: u8,
    preflag: bool,
    sfscale: bool,
    c1table: bool,
}

#[derive(Default)]
struct Side {
    main_begin: u16,
    scfsi: [u8; 2],
    gr: [[Granule; 2]; 2],
}

#[derive(Clone, Copy)]
struct Bands {
    n: usize,
    n_long: usize,
    width: [u8; 40],
    sf: [u8; 40],
}

fn read_side(f: &Frame, b: &mut Bits<'_>, s: &mut Side) -> Result<()> {
    let lsf = f.version != 0;
    let nch = usize::from(f.h.channels);
    s.main_begin = b.get(if lsf { 8 } else { 9 }) as u16;
    b.pos += match (lsf, nch) {
        (true, 1) => 1,
        (true, _) => 2,
        (false, 1) => 5,
        (false, _) => 3,
    };
    for ch in 0..nch {
        s.scfsi[ch] = if lsf { 0 } else { b.get(4) as u8 };
    }
    for gr in 0..(if lsf { 1 } else { 2 }) {
        for ch in 0..nch {
            let g = &mut s.gr[gr][ch];
            g.part23 = b.get(12) as u16;
            g.big_values = b.get(9) as u16;
            if g.big_values > 288 {
                return Err(Error::InvalidData);
            }
            g.global_gain = b.get(8) as u8;
            g.sfc = b.get(if lsf { 9 } else { 4 }) as u16;
            if b.get(1) != 0 {
                g.block_type = b.get(2) as u8;
                if g.block_type == 0 {
                    return Err(Error::InvalidData);
                }
                g.mixed = b.get(1) != 0;
                g.table[0] = b.get(5) as u8;
                g.table[1] = b.get(5) as u8;
                g.table[2] = 0;
                for w in 0..3 {
                    g.sbg[w] = b.get(3) as u8;
                }
                g.region0 = 0;
                g.region1 = 0;
            } else {
                g.block_type = 0;
                g.mixed = false;
                for r in 0..3 {
                    g.table[r] = b.get(5) as u8;
                }
                g.region0 = b.get(4) as u8;
                g.region1 = b.get(3) as u8;
                g.sbg = [0; 3];
            }
            g.preflag = !lsf && b.get(1) != 0;
            g.sfscale = b.get(1) != 0;
            g.c1table = b.get(1) != 0;
        }
    }
    Ok(())
}

fn build_bands(f: &Frame, g: &Granule, bb: &mut Bands) {
    let l = &SFB_LONG[usize::from(f.rate_index)];
    let s = &SFB_SHORT[usize::from(f.rate_index)];
    let mut n = 0;
    if g.block_type != 2 {
        bb.width[..22].copy_from_slice(l);
        bb.n = 22;
        bb.n_long = 22;
        return;
    }
    let mut first = 0;
    if g.mixed {
        let (mut i, mut lines) = (0, 0);
        while lines < 36 {
            bb.width[n] = l[i];
            n += 1;
            lines += i32::from(l[i]);
            i += 1;
        }
        first = 12;
    }
    bb.n_long = n;
    let mut start = 0;
    for &w in s {
        let mut lo = start;
        let hi = start + i32::from(w);
        start = hi;
        if hi <= first {
            continue;
        }
        lo = lo.max(first);
        bb.width[n..n + 3].fill((hi - lo) as u8);
        n += 3;
    }
    bb.n = n;
}

fn read_sf_mpeg1(prev: &mut [u8; 22], b: &mut Bits<'_>, g: &Granule, gr: usize, scfsi: u8, bb: &mut Bands) {
    let slen1 = u32::from(SLEN[usize::from(g.sfc)][0]);
    let slen2 = u32::from(SLEN[usize::from(g.sfc)][1]);
    let sf = &mut bb.sf;
    if g.block_type == 2 {
        let first = if g.mixed { 17 } else { 18 };
        let mut i = 0;
        while i < first {
            sf[i] = b.get(slen1) as u8;
            i += 1;
        }
        while i + 3 < bb.n {
            sf[i] = b.get(slen2) as u8;
            i += 1;
        }
        while i < bb.n {
            sf[i] = 0;
            i += 1;
        }
        return;
    }
    const GROUP: [usize; 5] = [0, 6, 11, 16, 21];
    for grp in 0..4 {
        let reuse = gr == 1 && (scfsi >> (3 - grp)) & 1 != 0;
        let slen = if grp < 2 { slen1 } else { slen2 };
        for i in GROUP[grp]..GROUP[grp + 1] {
            sf[i] = if reuse { prev[i] } else { b.get(slen) as u8 };
        }
    }
    sf[21] = 0;
    prev.copy_from_slice(&sf[..22]);
}

fn read_sf_lsf(b: &mut Bits<'_>, g: &mut Granule, is_right: bool, bb: &mut Bands) {
    let mut slen = [0u32; 4];
    let table;
    let mut sfc = u32::from(g.sfc);
    if !is_right {
        if sfc < 400 {
            slen[0] = (sfc >> 4) / 5;
            slen[1] = (sfc >> 4) % 5;
            slen[2] = (sfc & 15) >> 2;
            slen[3] = sfc & 3;
            table = 0;
        } else if sfc < 500 {
            sfc -= 400;
            slen[0] = (sfc >> 2) / 5;
            slen[1] = (sfc >> 2) % 5;
            slen[2] = sfc & 3;
            table = 1;
        } else {
            sfc -= 500;
            slen[0] = sfc / 3;
            slen[1] = sfc % 3;
            g.preflag = true;
            table = 2;
        }
    } else {
        sfc >>= 1;
        if sfc < 180 {
            slen[0] = sfc / 36;
            slen[1] = (sfc % 36) / 6;
            slen[2] = (sfc % 36) % 6;
            table = 3;
        } else if sfc < 244 {
            sfc -= 180;
            slen[0] = (sfc & 63) >> 4;
            slen[1] = (sfc & 15) >> 2;
            slen[2] = sfc & 3;
            table = 4;
        } else {
            sfc -= 244;
            slen[0] = sfc / 3;
            slen[1] = sfc % 3;
            table = 5;
        }
    }
    let blk = match (g.block_type == 2, g.mixed) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    };
    let nr = &LSF_NSFB[table][blk];
    let mut i = 0;
    for p in 0..4 {
        let mut j = 0;
        while j < nr[p] && i < bb.n {
            bb.sf[i] = b.get(slen[p]) as u8;
            i += 1;
            j += 1;
        }
    }
    bb.sf[i..bb.n].fill(0);
}

#[inline(always)]
fn huff(b: &mut Bits<'_>, lut: &[u16], root_bits: u8) -> u32 {
    let w = b.show();
    let mut bits = u32::from(root_bits);
    let mut base = 0;
    let mut e = u32::from(unsafe { *lut.get_unchecked((w >> (32 - bits)) as usize) });
    while e >> 15 != 0 {
        base += bits;
        bits = (e >> 12) & 7;
        let at = (e & 0xfff) + ((w << base) >> (32 - bits));
        e = u32::from(unsafe { *lut.get_unchecked(at as usize) });
    }
    b.pos += base + (e >> 9);
    e & 0x1ff
}

#[inline(always)]
fn signed_value(b: &mut Bits<'_>, v: u32, linbits: u8) -> i32 {
    let mut v = v;
    if v == 15 && linbits != 0 {
        v += b.get(u32::from(linbits));
    }
    if v == 0 {
        return 0;
    }
    if b.get(1) != 0 { -(v as i32) } else { v as i32 }
}

fn read_huffman(f: &Frame, b: &mut Bits<'_>, g: &Granule, part_end: u32, is: &mut [i32; 576]) -> usize {
    let l = &SFB_LONG[usize::from(f.rate_index)];
    let mut bound = [576usize; 3];
    if g.block_type != 0 {
        let s = &SFB_SHORT[usize::from(f.rate_index)];
        bound[0] = if g.block_type == 2 {
            3 * (usize::from(s[0]) + usize::from(s[1]) + usize::from(s[2]))
        } else {
            l[..8].iter().map(|&v| usize::from(v)).sum()
        };
    } else {
        let (mut line, mut i) = (0, 0);
        let region0 = usize::from(g.region0);
        let region1 = region0 + usize::from(g.region1) + 1;
        while i <= region0 && i < 22 {
            line += usize::from(l[i]);
            i += 1;
        }
        bound[0] = if i <= region0 { 576 } else { line };
        while i <= region1 && i < 22 {
            line += usize::from(l[i]);
            i += 1;
        }
        bound[1] = if i <= region1 { 576 } else { line };
    }

    let big = usize::from(g.big_values) * 2;
    let mut i = 0;
    for r in 0..3 {
        let end = bound[r].min(big);
        let h = &HUFF_PAIRS[usize::from(g.table[r])];
        if h.root_bits == 0 {
            if i < end {
                is[i..end].fill(0);
                i = end;
            }
            continue;
        }
        let lut = &HUFF_LUT[usize::from(h.offset)..];
        while i < end {
            let v = huff(b, lut, h.root_bits);
            is[i] = signed_value(b, v >> 4, h.linbits);
            is[i + 1] = signed_value(b, v & 15, h.linbits);
            i += 2;
        }
    }

    let quad = &HUFF_LUT[usize::from(HUFF_QUAD.offset)..];
    while i + 4 <= 576 && b.pos < part_end {
        let v = if g.c1table { 15 - b.get(4) } else { huff(b, quad, HUFF_QUAD.root_bits) };
        let mut q = [0i32; 4];
        for (k, q) in q.iter_mut().enumerate() {
            *q = signed_value(b, (v >> (3 - k)) & 1, 0);
        }
        if b.pos > part_end {
            break;
        }
        is[i..i + 4].copy_from_slice(&q);
        i += 4;
    }
    let nz = i;
    is[i..].fill(0);
    nz
}

#[inline(always)]
fn pow43_q17(q: i32) -> i64 {
    if q < 1024 {
        return i64::from(POW43_Q17[q as usize]);
    }
    let a = (q >> 3) as usize;
    let r = i64::from(q & 7);
    let lo = i64::from(POW43_Q17[a]);
    let hi = i64::from(POW43_Q17[a + 1]);
    2 * (8 * lo + r * (hi - lo))
}

fn requantize(g: &Granule, bb: &Bands, x: &mut [i32; 576], nz: usize) {
    let sh = if g.sfscale { 4 } else { 2 };
    let mut line = 0;
    for i in 0..bb.n {
        if line >= nz {
            break;
        }
        let mut e = i32::from(g.global_gain) - 210;
        if i < bb.n_long {
            let pre = if g.preflag { i32::from(PRETAB[i]) } else { 0 };
            e -= sh * (i32::from(bb.sf[i]) + pre);
        } else {
            e -= 8 * i32::from(g.sbg[(i - bb.n_long) % 3]) + sh * i32::from(bb.sf[i]);
        }
        let frac = i64::from(POW2_QUARTER_Q27[(e & 3) as usize]);
        let shift = (e >> 2) + FRAC as i32 - 17 - 27;
        let end = line + usize::from(bb.width[i]);
        for v in &mut x[line..end] {
            let q = *v;
            if q == 0 {
                continue;
            }
            let p = pow43_q17(q.abs()) * frac;
            let mut r = if shift >= 0 {
                if shift > 30 || p > (i64::from(XR_MAX) >> shift) { i64::from(XR_MAX) } else { p << shift }
            } else if -shift > 62 {
                0
            } else {
                (p + (1i64 << (-shift - 1))) >> -shift
            };
            r = r.min(i64::from(XR_MAX));
            *v = if q < 0 { -(r as i32) } else { r as i32 };
        }
        line = end;
    }
}

#[inline(always)]
fn clamp_xr(v: i64) -> i32 {
    v.clamp(-i64::from(XR_MAX), i64::from(XR_MAX)) as i32
}

fn mid_side(l: &mut [i32], r: &mut [i32]) {
    const INV_SQRT2: i64 = 1518500250;
    for (l, r) in l.iter_mut().zip(r) {
        let (m, s) = (i64::from(*l), i64::from(*r));
        *l = clamp_xr(((m + s) * INV_SQRT2) >> 31);
        *r = clamp_xr(((m - s) * INV_SQRT2) >> 31);
    }
}

fn stereo(f: &Frame, gr: &[Granule; 2], bands: &mut [Bands; 2], xr: &mut [[i32; 576]; 2], nz: &mut [usize; 2]) {
    let ms = f.mode_ext & 2 != 0;
    let is = f.mode_ext & 1 != 0;
    let n = nz[0].max(nz[1]);
    *nz = [n, n];
    let (x0, x1) = xr.split_at_mut(1);
    let (x0, x1) = (&mut x0[0], &mut x1[0]);
    if !is {
        if ms {
            mid_side(&mut x0[..n], &mut x1[..n]);
        }
        return;
    }

    let bb = &mut bands[1];
    let lsf = f.version != 0;
    let mut last = [-1i32; 4];
    let mut line = 0;
    for i in 0..bb.n {
        let w = usize::from(bb.width[i]);
        if x1[line..line + w].iter().any(|&v| v != 0) {
            last[if i < bb.n_long { 3 } else { (i - bb.n_long) % 3 }] = i as i32;
        }
        line += w;
    }
    let shorts = bb.n_long < bb.n;
    let long_is = !shorts || (last[0] < 0 && last[1] < 0 && last[2] < 0);
    let mut used = [false; 40];
    for i in 0..bb.n {
        let above = if i < bb.n_long {
            long_is && i as i32 > last[3]
        } else {
            i as i32 > last[(i - bb.n_long) % 3]
        };
        used[i] = above && (lsf || bb.sf[i] < 7);
    }
    let blocks = if shorts { 3 } else { 1 };
    for w in 0..blocks {
        let top = bb.n - blocks + w;
        let prev = top - blocks;
        if used[top] {
            used[top] = used[prev];
            bb.sf[top] = bb.sf[prev];
        }
    }

    let scale = u32::from(gr[1].sfc & 1);
    let mut line = 0;
    for i in 0..bb.n {
        let pos = u32::from(bb.sf[i]);
        let w = usize::from(bb.width[i]);
        let l = &mut x0[line..line + w];
        let r = &mut x1[line..line + w];
        line += w;
        if used[i] {
            let (kl, kr) = if !lsf {
                (IS_RATIO_Q30[2 * pos as usize], IS_RATIO_Q30[2 * pos as usize + 1])
            } else {
                let e = ((pos + 1) >> 1) << scale;
                let k = if e >> 2 > 30 { 0 } else { POW2_NEG_QUARTER_Q30[(e & 3) as usize] >> (e >> 2) };
                if pos & 1 != 0 { (k, 1 << 30) } else { (1 << 30, k) }
            };
            for (l, r) in l.iter_mut().zip(r) {
                let v = i64::from(*l);
                *l = ((v * i64::from(kl)) >> 30) as i32;
                *r = ((v * i64::from(kr)) >> 30) as i32;
            }
        } else if ms {
            mid_side(l, r);
        }
    }
}

fn reorder(bb: &Bands, x: &mut [i32; 576], tmp: &mut [i32; 576], mixed: bool) {
    let start = if mixed { 36 } else { 0 };
    let mut line = start;
    let mut f0 = if mixed { 12 } else { 0 };
    let mut i = bb.n_long;
    while i < bb.n {
        let w = usize::from(bb.width[i]);
        for win in 0..3 {
            for j in 0..w {
                let fl = f0 + j;
                tmp[18 * (fl / 6) + 6 * win + fl % 6] = x[line];
                line += 1;
            }
        }
        f0 += w;
        i += 3;
    }
    x[start..].copy_from_slice(&tmp[start..]);
}

fn antialias(x: &mut [i32; 576], sblimit: usize) {
    for sb in 1..sblimit {
        for i in 0..8 {
            let (lo, hi) = (18 * sb - 1 - i, 18 * sb + i);
            let (a, b) = (x[lo], x[hi]);
            x[lo] = clamp_xr(2 * i64::from(mulh(a, ALIAS_CS[i]) - mulh(b, ALIAS_CA[i])));
            x[hi] = clamp_xr(2 * i64::from(mulh(b, ALIAS_CS[i]) + mulh(a, ALIAS_CA[i])));
        }
    }
}

#[inline(always)]
fn clamp_z(v: i32) -> i32 {
    v.clamp(-Z_MAX, Z_MAX)
}

fn imdct36(input: &[i32], ov: &mut [i32], out: &mut [[i32; 32]; 18], col: usize, win: &[i32; 36]) {
    let mut xh = A16([0i16; 24]);
    let mut xl = A16([0i16; 24]);
    let mut z = [0i32; 18];
    let s = headroom(&input[..18], 27);
    let round = (1 << s) >> 1;
    for k in 0..18 {
        split(&mut xh[k], &mut xl[k], input[k] * (1 << s));
    }
    kernels::dot24(&mut z, &xh, &xl, &IMDCT18_SPLIT, 18);
    for v in &mut z {
        *v = clamp_z(2i32.wrapping_mul(*v).wrapping_add(round) >> s);
    }
    for n in 0..18 {
        let y = if n < 9 { z[n + 9] } else { -z[26 - n] };
        out[n][col] = (2 * mulh(y, win[n])).wrapping_add(ov[n]);
    }
    for n in 18..36 {
        let y = if n < 27 { -z[26 - n] } else { -z[n - 27] };
        ov[n - 18] = 2 * mulh(y, win[n]);
    }
}

fn imdct12x3(input: &[i32], ov: &mut [i32], out: &mut [[i32; 32]; 18], col: usize) {
    let win = &IMDCT_WINDOW[2];
    let mut buf = [0i32; 36];
    let mut x = [0i32; 18];
    let s = headroom(&input[..18], 27);
    let round = (1 << s) >> 1;
    for k in 0..18 {
        x[k] = input[k] * (1 << s);
    }
    for w in 0..3 {
        let mut z = [0i32; 6];
        for m in 0..6 {
            let c = &IMDCT6[6 * m..6 * m + 6];
            let mut acc = 0i32;
            for k in 0..6 {
                acc = acc.wrapping_add(mulh(x[6 * w + k], c[k]));
            }
            z[m] = clamp_z(2i32.wrapping_mul(acc).wrapping_add(round) >> s);
        }
        for n in 0..12 {
            let y = if n < 3 {
                z[n + 3]
            } else if n < 9 {
                -z[8 - n]
            } else {
                -z[n - 9]
            };
            buf[6 + 6 * w + n] += 2 * mulh(y, win[n]);
        }
    }
    for n in 0..18 {
        out[n][col] = buf[n].wrapping_add(ov[n]);
        ov[n] = clamp_z(buf[18 + n]);
    }
}

impl State {
    fn hybrid(&mut self, ch: usize, g: &Granule, nsb: usize) {
        let x = &self.xr[ch];
        let ov = &mut self.overlap[ch];
        let out = &mut self.sb[ch];
        for sb in 0..32 {
            let ov = &mut ov[18 * sb..18 * sb + 18];
            if sb >= nsb {
                for t in 0..18 {
                    out[t][sb] = ov[t];
                }
                ov.fill(0);
            } else {
                let bt = if g.mixed && sb < 2 { 0 } else { usize::from(g.block_type) };
                if bt == 2 {
                    imdct12x3(&x[18 * sb..], ov, out, sb);
                } else {
                    imdct36(&x[18 * sb..], ov, out, sb, &IMDCT_WINDOW[bt]);
                }
            }
            if sb & 1 != 0 {
                for t in (1..18).step_by(2) {
                    out[t][sb] = out[t][sb].wrapping_neg();
                }
            }
        }
    }

    pub(super) fn l3_decode(&mut self, pcm: &mut [i16]) -> Result<()> {
        let f = self.f;
        let nch = usize::from(f.h.channels);
        let lsf = f.version != 0;
        let bytes = usize::from(f.h.bytes);
        let mut b = Bits::new(&self.frame, if f.crc { 48 } else { 32 }, bytes as u32 * 8);
        let mut side = Side::default();
        read_side(&f, &mut b, &mut side)?;

        let start = (b.pos >> 3) as usize;
        if start > bytes {
            return Err(Error::InvalidSize);
        }
        let len = bytes - start;
        let reservoir = self.reservoir;
        self.main[reservoir..reservoir + len].copy_from_slice(&self.frame[start..bytes]);
        let total = reservoir + len;
        self.main[total..total + PAD].fill(0);
        let main_begin = usize::from(side.main_begin);
        let have = main_begin <= reservoir;
        let base = if have { reservoir - main_begin } else { 0 };
        let m_end = (total - base) as u32 * 8;

        let joint = f.mode == MODE_JOINT && nch == 2;
        let out_frame = 576 * self.channels;
        let mut pos = 0;
        for gr in 0..(if lsf { 1 } else { 2 }) {
            let mut bands = [Bands { n: 0, n_long: 0, width: [0; 40], sf: [0; 40] }; 2];
            let mut nz = [0usize; 2];
            let g = &mut side.gr[gr];
            for ch in 0..nch {
                build_bands(&f, &g[ch], &mut bands[ch]);
                if !have {
                    self.xr[ch] = [0; 576];
                    continue;
                }
                let mut m = Bits::new(&self.main[base..], pos, m_end);
                let end = pos + u32::from(g[ch].part23);
                if lsf {
                    read_sf_lsf(&mut m, &mut g[ch], joint && f.mode_ext & 1 != 0 && ch == 1, &mut bands[ch]);
                } else {
                    read_sf_mpeg1(&mut self.scf_prev[ch], &mut m, &g[ch], gr, side.scfsi[ch], &mut bands[ch]);
                }
                nz[ch] = read_huffman(&f, &mut m, &g[ch], end, &mut self.xr[ch]);
                requantize(&g[ch], &bands[ch], &mut self.xr[ch], nz[ch]);
                pos = end;
            }
            if joint && have {
                stereo(&f, g, &mut bands, &mut self.xr, &mut nz);
            }
            for ch in 0..nch {
                let mut nsb = 32;
                if g[ch].block_type == 2 {
                    reorder(&bands[ch], &mut self.xr[ch], &mut self.scratch, g[ch].mixed);
                    if g[ch].mixed {
                        antialias(&mut self.xr[ch], 2);
                    }
                } else {
                    let used = nz[ch].div_ceil(18);
                    nsb = if used < 31 { used + 1 } else { 32 };
                    antialias(&mut self.xr[ch], nsb);
                }
                let granule = g[ch];
                self.hybrid(ch, &granule, nsb);
            }
            self.emit(18, &mut pcm[gr * out_frame..]);
        }

        let keep = total.min(MAX_BACK);
        self.main.copy_within(total - keep..total, 0);
        self.reservoir = keep;
        Ok(())
    }
}
