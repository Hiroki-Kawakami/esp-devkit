/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

use devkit::{A16, Zeroable};
use idf::heap::{self, Caps};

use crate::mem::{Buffer, buffer, internal_buffer, state};
use crate::stream::Reader;
use crate::kernels;
use crate::{DecodeOptions, Error, HAVE_PIE, ImageInfo, PixelFormat, Result, Stream};

const FAST_BITS: i32 = 9;
const FAST_SIZE: usize = 1 << FAST_BITS;

#[repr(C)]
struct Huffman {
    bits: [u8; 17],
    vals: [u8; 256],
    mincode: [i32; 17],
    maxcode: [i32; 17],
    valptr: [i32; 17],
    fast: [u16; FAST_SIZE],
}

#[repr(C)]
struct Tables {
    qt: [[u16; 64]; 4],
    dqt: [[u16; 64]; 4],
    dc: [Huffman; 4],
    ac: [Huffman; 4],
}

unsafe impl Zeroable for Huffman {}
unsafe impl Zeroable for Tables {}

#[derive(Default)]
struct Component {
    id: i32,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    dcpred: i32,
    bw: usize,
    bh: usize,
    nbw: usize,
    nbh: usize,
    rows: Option<Vec<Buffer<i16>, Caps>>,
}

const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const IDCT_M: [[i16; 8]; 8] = [
    [5793, 5793, 5793, 5793, 5793, 5793, 5793, 5793],
    [8035, 6811, 4551, 1598, -1598, -4551, -6811, -8035],
    [7568, 3135, -3135, -7568, -7568, -3135, 3135, 7568],
    [6811, -1598, -8035, -4551, 4551, 8035, 1598, -6811],
    [5793, -5793, -5793, 5793, 5793, -5793, -5793, 5793],
    [4551, -8035, 1598, 6811, -6811, -1598, 8035, -4551],
    [3135, -7568, 7568, -3135, -3135, 7568, -7568, 3135],
    [1598, -4551, 6811, -8035, 8035, -6811, 4551, -1598],
];

const IDCT_A4: [i16; 8 * 4] = [
    5793, 5793, 5793, 5793,
    7423, 3075, -3075, -7423,
    5352, -5352, -5352, 5352,
    2607, -6293, 6293, -2607,
    0, 0, 0, 0,
    -1742, 4205, -4205, 1742,
    -2217, 2217, 2217, -2217,
    -1477, -612, 612, 1477,
];
const IDCT_A2: [i16; 8 * 2] = [
    5793, 5793,
    5249, -5249,
    0, 0,
    -1843, 1843,
    0, 0,
    1232, -1232,
    0, 0,
    -1044, 1044,
];

const IDCT_PASS1_BITS: u32 = 12;
const IDCT_PASS2_BITS: u32 = 16;

#[inline(always)]
fn clamp8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

#[inline(always)]
fn sat16(v: i32) -> i16 {
    if v < -32768 {
        -32768
    } else if v > 32767 {
        32767
    } else {
        v as i16
    }
}

fn idct8x8(dqc: &[i16; 64], rowmask: u32, out: &mut [i16; 64]) {
    for v in 0..8 {
        let row: &mut [i16; 8] = (&mut out[v * 8..v * 8 + 8]).try_into().unwrap();
        if rowmask & (1 << v) == 0 {
            *row = [0; 8];
            continue;
        }
        let input: &[i16; 8] = dqc[v * 8..v * 8 + 8].try_into().unwrap();
        for (x, cell) in row.iter_mut().enumerate() {
            let mut acc = 1 << (IDCT_PASS1_BITS - 1);
            for u in 0..8 {
                acc += i32::from(input[u]) * i32::from(IDCT_M[u][x]);
            }
            *cell = sat16(acc >> IDCT_PASS1_BITS);
        }
    }
    for x in 0..8 {
        let mut col = [0i16; 8];
        for v in 0..8 {
            col[v] = out[v * 8 + x];
        }
        for y in 0..8 {
            let mut acc = 1 << (IDCT_PASS2_BITS - 1);
            for v in 0..8 {
                acc += i32::from(col[v]) * i32::from(IDCT_M[v][y]);
            }
            out[y * 8 + x] = sat16(acc >> IDCT_PASS2_BITS);
        }
    }
}

unsafe fn idct8x8_u8(dqc: &A16<[i16; 64]>, rowmask: u32, dst: *mut u8, stride: usize) {
    if HAVE_PIE {
        unsafe { kernels::idct(dst, stride, dqc, rowmask, kernels::IdctSize::Full) };
        return;
    }
    let mut sp = [0; 64];
    idct8x8(dqc, rowmask, &mut sp);
    for y in 0..8 {
        for x in 0..8 {
            unsafe { *dst.add(y * stride + x) = clamp8(i32::from(sp[y * 8 + x]) + 128) };
        }
    }
}

unsafe fn idct_nxn_pie(dqc: &A16<[i16; 64]>, rowmask: u32, n: usize, dst: *mut u8, stride: usize) {
    let mut sp = A16([0u8; 64]);
    let size = if n == 4 { kernels::IdctSize::Half } else { kernels::IdctSize::Quarter };
    unsafe { kernels::idct(sp.as_mut_ptr(), 8, dqc, rowmask, size) };
    for y in 0..n {
        unsafe { ptr::copy_nonoverlapping(sp.as_ptr().add(y * 8), dst.add(y * stride), n) };
    }
}

unsafe fn idct_nxn(dqc: &A16<[i16; 64]>, rowmask: u32, n: usize, dst: *mut u8, stride: usize) {
    if HAVE_PIE {
        unsafe { idct_nxn_pie(dqc, rowmask, n, dst, stride) };
        return;
    }
    let a: &[i16] = if n == 4 { &IDCT_A4 } else { &IDCT_A2 };
    let mut t = [0i16; 8 * 4];
    for v in 0..8 {
        if rowmask & (1 << v) == 0 {
            continue;
        }
        let input = &dqc[v * 8..v * 8 + 8];
        for x in 0..n {
            let mut acc = 1 << (IDCT_PASS1_BITS - 1);
            for u in 0..8 {
                acc += i32::from(input[u]) * i32::from(a[u * n + x]);
            }
            t[v * n + x] = sat16(acc >> IDCT_PASS1_BITS);
        }
    }
    for y in 0..n {
        for x in 0..n {
            let mut acc = 1 << (IDCT_PASS2_BITS - 1);
            for v in 0..8 {
                acc += i32::from(t[v * n + x]) * i32::from(a[v * n + y]);
            }
            let value = clamp8(i32::from(sat16(acc >> IDCT_PASS2_BITS)) + 128);
            unsafe { *dst.add(y * stride + x) = value };
        }
    }
}

// The DC-only value, through the same two roundings as a full block so the
// shortcut cannot disagree with it.
#[inline]
fn idct_dc(dc: i16) -> u8 {
    let m = i32::from(IDCT_M[0][0]);
    let t = sat16((i32::from(dc) * m + (1 << (IDCT_PASS1_BITS - 1))) >> IDCT_PASS1_BITS);
    clamp8(i32::from(sat16((i32::from(t) * m + (1 << (IDCT_PASS2_BITS - 1))) >> IDCT_PASS2_BITS)) + 128)
}

unsafe fn idct_reduce(blk: usize, dqc: &A16<[i16; 64]>, dst: *mut u8, stride: usize, rowmask: u32, ac: bool) {
    if !ac {
        let v = idct_dc(dqc[0]);
        for y in 0..blk {
            unsafe { ptr::write_bytes(dst.add(y * stride), v, blk) };
        }
        return;
    }
    unsafe {
        match blk {
            8 => idct8x8_u8(dqc, rowmask, dst, stride),
            4 => idct_nxn(dqc, rowmask, 4, dst, stride),
            2 => idct_nxn(dqc, rowmask, 2, dst, stride),
            _ => *dst = idct_dc(dqc[0]),
        }
    }
}

struct Bits {
    buf: u32,
    count: i32,
    marker_pending: bool,
    marker: u8,
    err: bool,
}

impl Bits {
    const fn new() -> Self {
        Self { buf: 0, count: 0, marker_pending: false, marker: 0, err: false }
    }

    fn data_byte(&mut self, r: &mut Reader) -> u32 {
        if self.marker_pending {
            return 0;
        }
        let Some(b) = r.byte() else {
            self.marker_pending = true;
            self.marker = 0xD9;
            return 0;
        };
        if b != 0xFF {
            return u32::from(b);
        }
        let mut m = r.byte();
        while m == Some(0xFF) {
            m = r.byte();
        }
        match m {
            Some(0) => 0xFF,
            Some(m) => {
                self.marker_pending = true;
                self.marker = m;
                0
            }
            None => {
                self.marker_pending = true;
                self.marker = 0xD9;
                0
            }
        }
    }

    fn refill(&mut self, r: &mut Reader) {
        while self.count <= 24 {
            if !self.marker_pending && r.pos + 4 <= r.len {
                let p = unsafe { r.buf.as_ptr().add(r.pos).cast::<[u8; 4]>().read() };
                let w = u32::from_be_bytes(p);
                let x = !w;
                if x.wrapping_sub(0x0101_0101) & !x & 0x8080_8080 == 0 {
                    let k = (32 - self.count) >> 3;
                    self.buf = if k == 4 { w } else { (self.buf << (k * 8)) | (w >> (32 - k * 8)) };
                    self.count += k * 8;
                    r.pos += k as usize;
                    continue;
                }
            }
            let b = self.data_byte(r);
            self.buf = (self.buf << 8) | b;
            self.count += 8;
        }
    }

    #[inline(always)]
    fn fill(&mut self, r: &mut Reader, n: i32) {
        if self.count < n {
            self.refill(r);
        }
    }

    #[inline(always)]
    fn get(&mut self, r: &mut Reader, n: i32) -> i32 {
        if n == 0 {
            return 0;
        }
        self.fill(r, n);
        self.count -= n;
        ((self.buf >> self.count) & ((1u32 << n) - 1)) as i32
    }

    #[inline(always)]
    fn receive_extend(&mut self, r: &mut Reader, s: i32) -> i32 {
        let v = self.get(r, s);
        if v < 1 << (s - 1) { v - ((1 << s) - 1) } else { v }
    }

    #[cold]
    fn huffman_slow(&mut self, r: &mut Reader, h: &Huffman) -> i32 {
        let mut code = 0;
        for l in 1..=16 {
            code = (code << 1) | self.get(r, 1);
            if h.maxcode[l] >= 0 && code <= h.maxcode[l] {
                return i32::from(h.vals[((h.valptr[l] + code - h.mincode[l]) & 0xFF) as usize]);
            }
        }
        self.err = true;
        0
    }

    #[inline(always)]
    fn huffman(&mut self, r: &mut Reader, h: &Huffman) -> i32 {
        self.fill(r, FAST_BITS);
        let index = (self.buf >> (self.count - FAST_BITS)) as usize & (FAST_SIZE - 1);
        let e = unsafe { *h.fast.get_unchecked(index) };
        if e != 0 {
            self.count -= i32::from(e >> 8);
            return i32::from(e & 0xFF);
        }
        self.huffman_slow(r, h)
    }

    /// A DC category past 16 has no meaning, and would shift the buffer away.
    #[inline(always)]
    fn dc_diff(&mut self, r: &mut Reader, t: i32) -> i32 {
        if t == 0 {
            return 0;
        }
        if t > 16 {
            self.err = true;
            return 0;
        }
        self.receive_extend(r, t)
    }
}

/// Baseline and progressive 8-bit Huffman JPEG, grayscale and YCbCr up to 2x2
/// sampling, with a 1/1 to 1/8 downscale while decoding. A progressive stream is
/// read whole on the first `next_row`; see the README for what that costs.
pub struct JpegDecoder<'a> {
    r: Reader<'a>,
    t: Box<Tables, Caps>,
    bits: Bits,
    caps: Option<Caps>,

    w: usize,
    h: usize,
    width: u16,
    height: u16,
    comps: usize,
    comp: [Component; 3],
    hmax: usize,
    vmax: usize,
    restart_interval: u32,

    progressive: bool,
    prog_ready: bool,
    scan_comps: usize,
    scan_comp: [usize; 3],
    ss: i32,
    se: i32,
    ah: i32,
    al: i32,
    eobrun: i32,

    blk: usize,
    mcus_per_row: usize,
    mcu_rows: usize,
    mcu_row_idx: usize,
    mcu_count: u32,

    band: Buffer,
    plane: [usize; 3],
    pstride: [usize; 3],
    vsh: [u32; 3],
    hsh: [u32; 3],
    band_valid_rows: usize,
    band_row: usize,
    out_row: usize,
}

fn read_u16(r: &mut Reader) -> Option<i32> {
    let hi = r.byte()?;
    let lo = r.byte()?;
    Some((i32::from(hi) << 8) | i32::from(lo))
}

fn parse_dqt(r: &mut Reader, t: &mut Tables, mut len: i32) -> Result<()> {
    while len > 0 {
        let pq_tq = r.byte().ok_or(Error::Decode)?;
        let (pq, tq) = (pq_tq >> 4, usize::from(pq_tq & 0x0F));
        if tq > 3 {
            return Err(Error::Decode);
        }
        len -= 1;
        for k in 0..64 {
            let v = if pq != 0 { read_u16(r) } else { r.byte().map(i32::from) };
            t.qt[tq][k] = v.ok_or(Error::Decode)? as u16;
        }
        len -= if pq != 0 { 128 } else { 64 };
    }
    Ok(())
}

fn parse_dht(r: &mut Reader, tables: &mut Tables, mut len: i32) -> Result<()> {
    while len > 0 {
        let tc_th = r.byte().ok_or(Error::Decode)?;
        let (tc, th) = (tc_th >> 4, usize::from(tc_th & 0x0F));
        if tc > 1 || th > 3 {
            return Err(Error::Decode);
        }
        let t = if tc != 0 { &mut tables.ac[th] } else { &mut tables.dc[th] };
        let mut total = 0;
        t.bits[0] = 0;
        for i in 1..=16 {
            let c = r.byte().ok_or(Error::Decode)?;
            t.bits[i] = c;
            total += usize::from(c);
        }
        if total > 256 {
            return Err(Error::Decode);
        }
        if !r.read_exact(&mut t.vals[..total]) {
            return Err(Error::Decode);
        }
        let mut code = 0;
        let mut k = 0;
        for l in 1..=16 {
            if t.bits[l] != 0 {
                t.valptr[l] = k;
                t.mincode[l] = code;
                code += i32::from(t.bits[l]);
                // An over-subscribed table has no valid codes left at this
                // length; its mincode would index past the fast table.
                if code > 1 << l {
                    return Err(Error::Decode);
                }
                t.maxcode[l] = code - 1;
                k += i32::from(t.bits[l]);
            } else {
                t.maxcode[l] = -1;
            }
            code <<= 1;
        }
        t.fast = [0; FAST_SIZE];
        for l in 1..=FAST_BITS as usize {
            for i in 0..i32::from(t.bits[l]) {
                let base = ((t.mincode[l] + i) << (FAST_BITS as usize - l)) as usize;
                let cnt = 1usize << (FAST_BITS as usize - l);
                let entry = ((l as u16) << 8) | u16::from(t.vals[(t.valptr[l] + i) as usize]);
                t.fast[base..base + cnt].fill(entry);
            }
        }
        len -= 17 + total as i32;
    }
    Ok(())
}

impl<'a> JpegDecoder<'a> {
    pub fn open(src: impl Stream + 'a, options: &DecodeOptions) -> Result<Self> {
        let src: Box<dyn Stream + 'a> = Box::try_new(src).map_err(|_| Error::OutOfMemory)?;
        let mut d = Self {
            r: Reader::new(src)?,
            t: state()?,
            bits: Bits::new(),
            caps: options.caps,
            w: 0,
            h: 0,
            width: 0,
            height: 0,
            comps: 0,
            comp: Default::default(),
            hmax: 1,
            vmax: 1,
            restart_interval: 0,
            progressive: false,
            prog_ready: false,
            scan_comps: 0,
            scan_comp: [0; 3],
            ss: 0,
            se: 0,
            ah: 0,
            al: 0,
            eobrun: 0,
            blk: 8,
            mcus_per_row: 0,
            mcu_rows: 0,
            mcu_row_idx: 0,
            mcu_count: 0,
            band: Box::new_in([], Caps::DEFAULT),
            plane: [0; 3],
            pstride: [0; 3],
            vsh: [0; 3],
            hsh: [0; 3],
            band_valid_rows: 0,
            band_row: 0,
            out_row: 0,
        };
        d.parse_header(options)?;
        Ok(d)
    }

    pub fn info(&self) -> ImageInfo {
        let format = if self.comps == 1 { PixelFormat::Gray8 } else { PixelFormat::Rgb888 };
        ImageInfo { width: self.width, height: self.height, format }
    }

    fn parse_sof(&mut self, progressive: bool) -> Result<()> {
        self.progressive = progressive;
        let r = &mut self.r;
        if r.byte() != Some(8) {
            return Err(Error::Unsupported);
        }
        let h = read_u16(r).unwrap_or(-1);
        let w = read_u16(r).unwrap_or(-1);
        let nc = r.byte();
        if h <= 0 || w <= 0 {
            return Err(Error::Decode);
        }
        let nc = match nc {
            Some(n @ (1 | 3)) => usize::from(n),
            _ => return Err(Error::Unsupported),
        };
        self.h = h as usize;
        self.w = w as usize;
        self.comps = nc;
        self.hmax = 1;
        self.vmax = 1;
        for c in &mut self.comp[..nc] {
            let (Some(id), Some(hv), Some(tq)) = (r.byte(), r.byte(), r.byte()) else {
                return Err(Error::Decode);
            };
            if tq > 3 {
                return Err(Error::Decode);
            }
            c.id = i32::from(id);
            c.h = usize::from(hv >> 4);
            c.v = usize::from(hv & 0x0F);
            c.tq = usize::from(tq);
            if !(1..=2).contains(&c.h) || !(1..=2).contains(&c.v) {
                return Err(Error::Unsupported);
            }
            self.hmax = self.hmax.max(c.h);
            self.vmax = self.vmax.max(c.v);
        }
        Ok(())
    }

    fn parse_sos(&mut self) -> Result<()> {
        let r = &mut self.r;
        let ns = r.byte().map_or(0, usize::from);
        if ns < 1 || ns > self.comps {
            return Err(Error::Decode);
        }
        if !self.progressive && ns != self.comps {
            return Err(Error::Unsupported);
        }
        self.scan_comps = ns;
        for i in 0..ns {
            let (Some(cs), Some(td_ta)) = (r.byte(), r.byte()) else {
                return Err(Error::Decode);
            };
            let ci = self.comp[..self.comps].iter().rposition(|c| c.id == i32::from(cs)).ok_or(Error::Decode)?;
            self.scan_comp[i] = ci;
            self.comp[ci].td = usize::from(td_ta >> 4);
            self.comp[ci].ta = usize::from(td_ta & 0x0F);
        }
        let (Some(ss), Some(se), Some(ahal)) = (r.byte(), r.byte(), r.byte()) else {
            return Err(Error::Decode);
        };
        self.ss = i32::from(ss);
        self.se = i32::from(se);
        self.ah = i32::from(ahal >> 4);
        self.al = i32::from(ahal & 0x0F);
        if self.progressive
            && (self.se > 63
                || self.ss > self.se
                || (self.ss == 0 && self.se != 0)
                || (self.ss != 0 && ns != 1)
                || self.ah > 13
                || self.al > 13)
        {
            return Err(Error::Decode);
        }
        // A progressive scan only reads the table its spectral range asks for,
        // and encoders leave junk in the other selector.
        let need_dc = !self.progressive || self.ss == 0;
        let need_ac = !self.progressive || self.se != 0;
        for i in 0..ns {
            let c = &self.comp[self.scan_comp[i]];
            if (need_dc && c.td > 3) || (need_ac && c.ta > 3) {
                return Err(Error::Decode);
            }
        }
        Ok(())
    }

    fn build_dq_tables(&mut self) {
        for c in &self.comp[..self.comps] {
            for k in 0..64 {
                self.t.dqt[c.tq][usize::from(ZIGZAG[k])] = self.t.qt[c.tq][k];
            }
        }
    }

    fn setup(&mut self, options: &DecodeOptions) -> Result<()> {
        let (w, h) = (self.w, self.h);
        let mut scale = 1;
        if options.target_width > 0 && options.target_height > 0 {
            while scale < 8
                && w / (scale * 2) >= usize::from(options.target_width)
                && h / (scale * 2) >= usize::from(options.target_height)
            {
                scale *= 2;
            }
        }
        if options.max_src_pixels != 0 {
            while scale < 8 {
                let pixels = w.div_ceil(scale) as u32 * h.div_ceil(scale) as u32;
                if pixels <= options.max_src_pixels {
                    break;
                }
                scale *= 2;
            }
        }
        self.blk = 8 / scale;
        let (hmax, vmax) = (self.hmax, self.vmax);
        self.mcus_per_row = w.div_ceil(8 * hmax);
        self.mcu_rows = h.div_ceil(8 * vmax);
        self.width = w.div_ceil(scale) as u16;
        self.height = h.div_ceil(scale) as u16;

        let mut band_bytes = 0;
        for i in 0..self.comps {
            let c = &mut self.comp[i];
            c.bw = self.mcus_per_row * c.h;
            c.bh = self.mcu_rows * c.v;
            c.nbw = (w * c.h).div_ceil(hmax).div_ceil(8);
            c.nbh = (h * c.v).div_ceil(vmax).div_ceil(8);
            self.vsh[i] = u32::from(c.v != vmax);
            self.hsh[i] = u32::from(c.h != hmax);
            self.pstride[i] = self.mcus_per_row * c.h * self.blk;
            self.plane[i] = band_bytes;
            band_bytes += self.pstride[i] * (c.v * self.blk);
        }
        // The SIMD colour conversion reads past the last row.
        band_bytes += 16;
        // Internal RAM is the fast place for the band, but only when the caller
        // left the choice open: explicit caps are a memory budget.
        let internal = if self.caps.is_none() && band_bytes <= 96 * 1024 { internal_buffer(band_bytes).ok() } else { None };
        self.band = match internal {
            Some(band) => band,
            None => buffer(band_bytes, self.caps)?,
        };
        self.build_dq_tables();
        Ok(())
    }

    fn parse_header(&mut self, options: &DecodeOptions) -> Result<()> {
        if self.r.byte() != Some(0xFF) || self.r.byte() != Some(0xD8) {
            return Err(Error::Decode);
        }
        let mut have_sof = false;
        loop {
            let b = self.r.byte().ok_or(Error::Truncated)?;
            if b != 0xFF {
                continue;
            }
            let mut marker = self.r.byte();
            while marker == Some(0xFF) {
                marker = self.r.byte();
            }
            let marker = marker.ok_or(Error::Truncated)?;
            match marker {
                0xD9 => return Err(Error::Decode),
                0xD0..=0xD7 | 0x01 => continue,
                _ => {}
            }
            let len = read_u16(&mut self.r).unwrap_or(-1);
            if len < 2 {
                return Err(Error::Decode);
            }
            let len = len - 2;
            match marker {
                0xDB => parse_dqt(&mut self.r, &mut self.t, len)?,
                0xC4 => parse_dht(&mut self.r, &mut self.t, len)?,
                0xC0 | 0xC2 => {
                    self.parse_sof(marker == 0xC2)?;
                    have_sof = true;
                }
                0xDD => self.restart_interval = read_u16(&mut self.r).unwrap_or(0) as u32,
                0xDA => {
                    if !have_sof {
                        return Err(Error::Decode);
                    }
                    self.parse_sos()?;
                    break;
                }
                0xC1..=0xCF if marker != 0xC8 => return Err(Error::Unsupported),
                _ => {
                    if !self.r.skip(len as usize) {
                        return Err(Error::Truncated);
                    }
                }
            }
        }
        self.setup(options)
    }

    fn consume_restart(&mut self) -> bool {
        self.bits.count = 0;
        self.bits.buf = 0;
        if self.bits.marker_pending {
            self.bits.marker_pending = false;
            return (0xD0..=0xD7).contains(&self.bits.marker);
        }
        loop {
            match self.r.byte() {
                None => return false,
                Some(0xFF) => {
                    let mut m = self.r.byte();
                    while m == Some(0xFF) {
                        m = self.r.byte();
                    }
                    return m.is_some_and(|m| (0xD0..=0xD7).contains(&m));
                }
                Some(_) => {}
            }
        }
    }

    fn band_row_extent(&mut self) {
        let base = self.mcu_row_idx * self.blk * self.vmax;
        self.band_valid_rows = (self.blk * self.vmax).min(usize::from(self.height).saturating_sub(base));
    }

    fn decode_mcu_row(&mut self) {
        self.band_row_extent();
        let band = self.band.as_mut_ptr();
        let blk = self.blk;
        for mx in 0..self.mcus_per_row {
            if self.bits.err {
                break;
            }
            if self.restart_interval != 0 && self.mcu_count > 0 && self.mcu_count % self.restart_interval == 0 {
                if !self.consume_restart() {
                    self.bits.err = true;
                    return;
                }
                self.comp[..self.comps].iter_mut().for_each(|c| c.dcpred = 0);
            }
            for ci in 0..self.comps {
                let c = &mut self.comp[ci];
                let stride = self.pstride[ci];
                let base = unsafe { band.add(self.plane[ci] + mx * c.h * blk) };
                for by in 0..c.v {
                    for bx in 0..c.h {
                        let dst = unsafe { base.add(by * blk * stride + bx * blk) };
                        unsafe { decode_block(&mut self.bits, &mut self.r, &self.t, c, blk, dst, stride) };
                    }
                }
            }
            self.mcu_count = self.mcu_count.wrapping_add(1);
        }
        self.band_row = 0;
        self.mcu_row_idx += 1;
    }

    #[inline]
    fn scan_restart(&mut self, n: u32) -> bool {
        if self.restart_interval == 0 || n == 0 || n % self.restart_interval != 0 {
            return true;
        }
        self.restart_scan()
    }

    #[cold]
    fn restart_scan(&mut self) -> bool {
        if !self.consume_restart() {
            self.bits.err = true;
            return false;
        }
        self.comp[..self.comps].iter_mut().for_each(|c| c.dcpred = 0);
        self.eobrun = 0;
        true
    }

    #[inline]
    fn decode_block_prog(&mut self, ci: usize, blk: &mut [i16]) {
        let Self { r, t, bits, comp, ss, se, ah, al, eobrun, .. } = self;
        let c = &mut comp[ci];
        let (dc, ac) = (&t.dc[c.td & 3], &t.ac[c.ta & 3]);
        let blk: &mut [i16; 64] = blk.try_into().unwrap();
        let scan = Scan { ss: *ss, se: *se, al: *al };
        match (*ss == 0, *ah == 0) {
            (true, true) => {
                let t = bits.huffman(r, dc);
                if bits.err {
                    return;
                }
                let diff = bits.dc_diff(r, t);
                c.dcpred = c.dcpred.wrapping_add(diff);
                blk[0] = c.dcpred.wrapping_mul(1 << *al) as i16;
            }
            (true, false) => {
                if bits.get(r, 1) != 0 {
                    blk[0] |= 1 << *al;
                }
            }
            (false, true) => prog_ac_first(bits, r, ac, &scan, eobrun, blk),
            (false, false) => prog_ac_refine(bits, r, ac, &scan, eobrun, blk),
        }
    }

    fn decode_scan(&mut self) {
        self.bits.buf = 0;
        self.bits.count = 0;
        self.eobrun = 0;
        self.comp[..self.comps].iter_mut().for_each(|c| c.dcpred = 0);

        // Out of `comp` for the length of the scan: a restart resets it.
        let mut rows = [const { None }; 3];
        for (out, c) in rows.iter_mut().zip(&mut self.comp) {
            *out = c.rows.take();
        }
        self.scan_blocks(&mut rows);
        for (c, row) in self.comp.iter_mut().zip(rows) {
            c.rows = row;
        }
    }

    fn scan_blocks(&mut self, rows: &mut [Option<Vec<Buffer<i16>, Caps>>; 3]) {
        let mut n = 0;
        if self.scan_comps == 1 {
            // Non-interleaved: the scan walks the component's own block grid, so
            // the blocks padding the last MCU are not coded.
            let ci = self.scan_comp[0];
            let (nbh, nbw) = (self.comp[ci].nbh, self.comp[ci].nbw);
            let rows = rows[ci].as_mut().unwrap();
            for row in &mut rows[..nbh] {
                for blk in row.chunks_exact_mut(64).take(nbw) {
                    if self.bits.err || !self.scan_restart(n) {
                        return;
                    }
                    self.decode_block_prog(ci, blk);
                    n += 1;
                }
            }
            return;
        }
        for my in 0..self.mcu_rows {
            for mx in 0..self.mcus_per_row {
                if self.bits.err || !self.scan_restart(n) {
                    return;
                }
                for i in 0..self.scan_comps {
                    let ci = self.scan_comp[i];
                    let (h, v) = (self.comp[ci].h, self.comp[ci].v);
                    let rows = rows[ci].as_mut().unwrap();
                    for by in 0..v {
                        for bx in 0..h {
                            let at = (mx * h + bx) * 64;
                            self.decode_block_prog(ci, &mut rows[my * v + by][at..at + 64]);
                        }
                    }
                }
                n += 1;
            }
        }
    }

    fn next_marker(&mut self) -> Option<u8> {
        loop {
            let m = if self.bits.marker_pending {
                self.bits.marker_pending = false;
                self.bits.marker
            } else {
                if self.r.byte()? != 0xFF {
                    continue;
                }
                let mut m = self.r.byte();
                while m == Some(0xFF) {
                    m = self.r.byte();
                }
                match m? {
                    0 => continue,
                    m => m,
                }
            };
            if !(0xD0..=0xD7).contains(&m) {
                return Some(m);
            }
        }
    }

    // One allocation per block row rather than one per component: a 5 Mpx frame
    // wants 10 MB for its luma coefficients alone, and a heap that has the bytes
    // free in total rarely has them in one piece.
    fn prog_alloc(&mut self) -> Result<()> {
        for c in &mut self.comp[..self.comps] {
            let caps = self.caps.unwrap_or_else(crate::default_caps);
            let mut rows = heap::try_vec(c.bh, caps)?;
            for _ in 0..c.bh {
                rows.push(buffer(c.bw * 64, self.caps)?);
            }
            c.rows = Some(rows);
        }
        Ok(())
    }

    fn prog_decode(&mut self) -> Result<()> {
        self.prog_alloc()?;
        let mut more = true;
        while more {
            self.decode_scan();
            if self.bits.err {
                break;
            }
            loop {
                let marker = match self.next_marker() {
                    None | Some(0xD9) => {
                        more = false;
                        break;
                    }
                    Some(0x01) => continue,
                    Some(marker) => marker,
                };
                let len = read_u16(&mut self.r).unwrap_or(-1);
                if len < 2 {
                    more = false;
                    break;
                }
                let len = len - 2;
                let ok = match marker {
                    0xDA => {
                        more = self.parse_sos().is_ok();
                        break;
                    }
                    0xC4 => parse_dht(&mut self.r, &mut self.t, len).is_ok(),
                    0xDB => parse_dqt(&mut self.r, &mut self.t, len).is_ok(),
                    0xDD => {
                        self.restart_interval = read_u16(&mut self.r).unwrap_or(0) as u32;
                        true
                    }
                    _ => self.r.skip(len as usize),
                };
                if !ok {
                    more = false;
                    break;
                }
            }
        }
        self.bits.err = false;
        self.build_dq_tables();
        Ok(())
    }

    fn output_mcu_row_prog(&mut self) {
        self.band_row_extent();
        let band = self.band.as_mut_ptr();
        let blk = self.blk;
        for ci in 0..self.comps {
            let c = &self.comp[ci];
            let stride = self.pstride[ci];
            let q = &self.t.dqt[c.tq];
            let rows = c.rows.as_ref().unwrap();
            for by in 0..c.v {
                let row = &rows[self.mcu_row_idx * c.v + by];
                let out = unsafe { band.add(self.plane[ci] + by * blk * stride) };
                for bx in 0..c.bw {
                    let src: &[i16; 64] = row[bx * 64..bx * 64 + 64].try_into().unwrap();
                    let mut dqc = A16([0i16; 64]);
                    let mut rowmask = 1;
                    let mut ac = false;
                    dqc[0] = sat16(i32::from(src[0]) * i32::from(q[0]));
                    for i in 1..64 {
                        if src[i] != 0 {
                            dqc[i] = sat16(i32::from(src[i]) * i32::from(q[i]));
                            rowmask |= 1 << (i >> 3);
                            ac = true;
                        }
                    }
                    unsafe { idct_reduce(blk, &dqc, out.add(bx * blk), stride, rowmask, ac) };
                }
            }
        }
        self.band_row = 0;
        self.mcu_row_idx += 1;
    }

    // Single-rounding Q8, so that the SIMD kernel can match it exactly.
    fn emit_row(&self, py: usize, dst: &mut [u8]) {
        let w = usize::from(self.width);
        let plane = |i: usize| self.band[self.plane[i] + (py >> self.vsh[i]) * self.pstride[i]..].as_ptr();
        let yr = plane(0);
        if self.comps == 1 {
            unsafe { ptr::copy_nonoverlapping(yr, dst.as_mut_ptr(), w) };
            return;
        }
        let (cbr, crr) = (plane(1), plane(2));
        let (hsh1, hsh2) = (self.hsh[1], self.hsh[2]);
        let mut x = 0;
        let mut out = dst.as_mut_ptr();
        if HAVE_PIE && hsh1 == hsh2 && w >> 4 > 0 {
            let n = w >> 4;
            unsafe { kernels::ycc_rgb(out, yr, cbr, crr, n, hsh1 != 0) };
            x = n << 4;
            out = unsafe { out.add(x * 3) };
        }
        while x < w {
            unsafe {
                let y0 = i32::from(*yr.add(x)) << 8;
                let cb = i32::from(*cbr.add(x >> hsh1)) - 128;
                let cr = i32::from(*crr.add(x >> hsh2)) - 128;
                *out = clamp8((y0 + 359 * cr) >> 8);
                *out.add(1) = clamp8((y0 - 88 * cb - 183 * cr) >> 8);
                *out.add(2) = clamp8((y0 + 454 * cb) >> 8);
                out = out.add(3);
            }
            x += 1;
        }
    }

    /// Writes a row and returns `true`; `false` at the end of the image.
    pub fn next_row(&mut self, dst: &mut [u8]) -> Result<bool> {
        if dst.len() < self.info().row_bytes() {
            return Err(Error::InvalidArgument);
        }
        if self.bits.err {
            return Err(Error::Truncated);
        }
        if self.progressive && !self.prog_ready {
            if let Err(e) = self.prog_decode() {
                self.bits.err = true;
                return Err(e);
            }
            self.prog_ready = true;
        }
        if self.out_row >= usize::from(self.height) {
            return Ok(false);
        }
        if self.band_row >= self.band_valid_rows {
            if self.mcu_row_idx >= self.mcu_rows {
                return Ok(false);
            }
            if self.progressive {
                self.output_mcu_row_prog();
            } else {
                self.decode_mcu_row();
            }
            if self.bits.err {
                return Err(Error::Truncated);
            }
        }
        self.emit_row(self.band_row, dst);
        self.band_row += 1;
        self.out_row += 1;
        Ok(true)
    }
}

unsafe fn decode_block(bits: &mut Bits, r: &mut Reader, t: &Tables, c: &mut Component, blk: usize, dst: *mut u8, stride: usize) {
    let q = &t.dqt[c.tq];
    let dc = unsafe { t.dc.get_unchecked(c.td & 3) };
    let ac = unsafe { t.ac.get_unchecked(c.ta & 3) };
    let mut dqc = A16([0i16; 64]);

    let s = bits.huffman(r, dc);
    let diff = bits.dc_diff(r, s);
    c.dcpred = c.dcpred.wrapping_add(diff);
    dqc[0] = sat16(c.dcpred.wrapping_mul(i32::from(q[0])));

    let mut rowmask = 1u32;
    let mut has_ac = false;
    let mut k = 1;
    while k < 64 {
        let rs = bits.huffman(r, ac);
        if bits.err {
            return;
        }
        let (run, s) = (rs >> 4, rs & 0x0F);
        if s == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break;
        }
        k += run as usize;
        if k >= 64 {
            break;
        }
        let nat = usize::from(unsafe { *ZIGZAG.get_unchecked(k) });
        let v = bits.receive_extend(r, s);
        unsafe {
            *dqc.get_unchecked_mut(nat) = sat16(v * i32::from(*q.get_unchecked(nat)));
        }
        rowmask |= 1 << (nat >> 3);
        has_ac = true;
        k += 1;
    }
    unsafe { idct_reduce(blk, &dqc, dst, stride, rowmask, has_ac) };
}

/// `k` is 0..=63: the scan's spectral range was checked when it was parsed.
#[inline(always)]
unsafe fn zigzag_mut(blk: &mut [i16; 64], k: i32) -> &mut i16 {
    unsafe { blk.get_unchecked_mut(usize::from(*ZIGZAG.get_unchecked(k as usize))) }
}

struct Scan {
    ss: i32,
    se: i32,
    al: i32,
}

fn prog_ac_first(bits: &mut Bits, r: &mut Reader, ac: &Huffman, scan: &Scan, eobrun: &mut i32, blk: &mut [i16; 64]) {
    if *eobrun > 0 {
        *eobrun -= 1;
        return;
    }
    let mut k = scan.ss;
    loop {
        let rs = bits.huffman(r, ac);
        if bits.err {
            return;
        }
        let (run, s) = (rs >> 4, rs & 0x0F);
        if s == 0 {
            if run < 15 {
                *eobrun = (1 << run) - 1;
                if run != 0 {
                    *eobrun += bits.get(r, run);
                }
                break;
            }
            k += 16;
        } else {
            k += run;
            if k > scan.se {
                bits.err = true;
                return;
            }
            let v = bits.receive_extend(r, s);
            *unsafe { zigzag_mut(blk, k) } = v.wrapping_mul(1 << scan.al) as i16;
            k += 1;
        }
        if k > scan.se {
            break;
        }
    }
}

fn prog_ac_refine(bits: &mut Bits, r: &mut Reader, ac: &Huffman, scan: &Scan, eobrun: &mut i32, blk: &mut [i16; 64]) {
    let p1 = 1 << scan.al;
    let m1 = -p1;
    let refine = |v: i16, sign: i32| v.wrapping_add((if v > 0 { p1 } else { sign }) as i16);
    let mut k = scan.ss;

    if *eobrun > 0 {
        *eobrun -= 1;
        while k <= scan.se {
            let p = unsafe { zigzag_mut(blk, k) };
            if *p != 0 && bits.get(r, 1) != 0 && i32::from(*p) & p1 == 0 {
                *p = refine(*p, m1);
            }
            k += 1;
        }
        return;
    }

    loop {
        let rs = bits.huffman(r, ac);
        if bits.err {
            return;
        }
        let (mut run, mut s) = (rs >> 4, rs & 0x0F);
        if s == 0 {
            if run < 15 {
                *eobrun = (1 << run) - 1;
                if run != 0 {
                    *eobrun += bits.get(r, run);
                }
                run = 64;
            }
        } else {
            if s != 1 {
                bits.err = true;
                return;
            }
            s = if bits.get(r, 1) != 0 { p1 } else { m1 };
        }
        while k <= scan.se {
            let p = unsafe { zigzag_mut(blk, k) };
            k += 1;
            if *p != 0 {
                if bits.get(r, 1) != 0 && i32::from(*p) & p1 == 0 {
                    *p = refine(*p, m1);
                }
            } else {
                if run == 0 {
                    if s != 0 {
                        *p = s as i16;
                    }
                    break;
                }
                run -= 1;
            }
        }
        if k > scan.se {
            break;
        }
    }
}
