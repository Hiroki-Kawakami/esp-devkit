/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit::Zeroable;
use devkit::math::lround;
use idf::heap::Caps;

use crate::mem::{Buffer, buffer, internal_buffer, state};
use crate::stream::Sink;
use crate::{Encoder, Error, ImageInfo, PixelFormat, Result};

const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const STD_Q_L: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61,
    12, 12, 14, 19, 26, 58, 60, 55,
    14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68, 109, 103, 77,
    24, 35, 55, 64, 81, 104, 113, 92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103, 99,
];
const STD_Q_C: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

const DC_L_BITS: [u8; 17] = [0, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_C_BITS: [u8; 17] = [0, 0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_L_BITS: [u8; 17] = [0, 0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_L_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const AC_C_BITS: [u8; 17] = [0, 0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_C_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

const K_0_382: i32 = 1567;
const K_0_541: i32 = 2217;
const K_0_707: i32 = 2896;
const K_1_306: i32 = 5352;

const AAN_SCALE: [f64; 8] = [
    1.0,
    1.387039845322148,
    1.306562964876377,
    1.175875602419359,
    1.0,
    0.785694958387102,
    0.541196100146197,
    0.275899379282943,
];

const STAGING_BYTES: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Subsampling {
    /// 4:2:0 for color.
    #[default]
    Default,
    S444,
    S422,
    S420,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct JpegOptions {
    /// 1..=100; 0 means 75.
    pub quality: u8,
    /// Ignored for gray.
    pub subsampling: Subsampling,
    pub caps: Option<Caps>,
}

/// Worst-case encoded size: header plus 2 bytes per sample, for very low quality.
pub const fn jpeg_buffer_upper_bound(w: u16, h: u16, format: PixelFormat) -> usize {
    let n = if matches!(format, PixelFormat::Gray8) { 1 } else { 3 };
    w as usize * h as usize * n * 2 + 1024
}

#[repr(C)]
struct Tables {
    qt_byte: [[u8; 64]; 2],
    qt_aan: [[i32; 64]; 2],
    hdc_code: [[u16; 16]; 2],
    hdc_len: [[u8; 16]; 2],
    hac_code: [[u16; 256]; 2],
    hac_len: [[u8; 256]; 2],
}

unsafe impl Zeroable for Tables {}

struct Core {
    width: usize,
    height: usize,
    comps: usize,
    sub_h: usize,
    sub_v: usize,
    mcu_w: usize,
    mcu_h: usize,
    width_pad: usize,
    input_565: bool,
    input_bgr: bool,

    t: Box<Tables, Caps>,
    blocks: Buffer<i16>,
    rows_in_buf: usize,
    total_rows: usize,
    dc_pred: [i32; 3],

    bitbuf: u32,
    bitcnt: u32,
    staging: Buffer,
    staged: usize,
    total: usize,
    failed: Option<Error>,
}

fn build_huffman_codes(bits: &[u8; 17], vals: &[u8], codes: &mut [u16], lens: &mut [u8]) {
    let mut code = 0u16;
    let mut k = 0;
    for len in 1..=16 {
        for _ in 0..bits[len] {
            let sym = usize::from(vals[k]);
            k += 1;
            codes[sym] = code;
            lens[sym] = len as u8;
            code = code.wrapping_add(1);
        }
        code = code.wrapping_shl(1);
    }
}

fn scale_table(std: &[u8; 64], out: &mut [u8; 64], quality: i32) {
    let scale = if quality < 50 { 5000 / quality } else { 200 - 2 * quality };
    for (o, &s) in out.iter_mut().zip(std) {
        *o = ((i32::from(s) * scale + 50) / 100).clamp(1, 255) as u8;
    }
}

#[inline(always)]
fn aan_mult(v: i32, c: i32) -> i32 {
    (v * c) >> 12
}

#[inline(always)]
fn fdct_1d(d: &mut [i32; 8]) {
    let (tmp0, tmp7) = (d[0] + d[7], d[0] - d[7]);
    let (tmp1, tmp6) = (d[1] + d[6], d[1] - d[6]);
    let (tmp2, tmp5) = (d[2] + d[5], d[2] - d[5]);
    let (tmp3, tmp4) = (d[3] + d[4], d[3] - d[4]);

    let (tmp10, tmp13) = (tmp0 + tmp3, tmp0 - tmp3);
    let (tmp11, tmp12) = (tmp1 + tmp2, tmp1 - tmp2);

    d[0] = tmp10 + tmp11;
    d[4] = tmp10 - tmp11;
    let z1 = aan_mult(tmp12 + tmp13, K_0_707);
    d[2] = tmp13 + z1;
    d[6] = tmp13 - z1;

    let tmp10 = tmp4 + tmp5;
    let tmp11 = tmp5 + tmp6;
    let tmp12 = tmp6 + tmp7;

    let z5 = aan_mult(tmp10 - tmp12, K_0_382);
    let z2 = aan_mult(tmp10, K_0_541) + z5;
    let z4 = aan_mult(tmp12, K_1_306) + z5;
    let z3 = aan_mult(tmp11, K_0_707);

    let z11 = tmp7 + z3;
    let z13 = tmp7 - z3;

    d[5] = z13 + z2;
    d[3] = z13 - z2;
    d[1] = z11 + z4;
    d[7] = z11 - z4;
}

fn fdct_8x8(block: &mut [i32; 64]) {
    for y in 0..8 {
        let row: &mut [i32; 8] = (&mut block[y * 8..y * 8 + 8]).try_into().unwrap();
        fdct_1d(row);
    }
    for x in 0..8 {
        let mut col = [0; 8];
        for i in 0..8 {
            col[i] = block[i * 8 + x];
        }
        fdct_1d(&mut col);
        for i in 0..8 {
            block[i * 8 + x] = col[i];
        }
    }
}

#[inline(always)]
fn magnitude(v: i32) -> u32 {
    32 - v.unsigned_abs().leading_zeros()
}

#[inline(always)]
fn extend_encode(v: i32, nbits: u32) -> u32 {
    let v = if v < 0 { v - 1 } else { v };
    v as u32 & ((1 << nbits) - 1)
}

#[inline(always)]
fn sdiv(v: i32, q: i32) -> i32 {
    if v >= 0 { (v + q / 2) / q } else { -((-v + q / 2) / q) }
}

impl Core {
    #[cold]
    fn drain(&mut self, sink: &mut dyn Sink) {
        if self.staged == 0 || self.failed.is_some() {
            return;
        }
        match sink.write(&self.staging[..self.staged]) {
            Ok(()) => {
                self.total += self.staged;
                self.staged = 0;
            }
            Err(e) => self.failed = Some(e),
        }
    }

    #[inline(always)]
    fn put_byte(&mut self, sink: &mut dyn Sink, b: u8) {
        if self.staged >= STAGING_BYTES {
            self.drain(sink);
            if self.staged >= STAGING_BYTES {
                return;
            }
        }
        unsafe { *self.staging.get_unchecked_mut(self.staged) = b };
        self.staged += 1;
    }

    fn put_word(&mut self, sink: &mut dyn Sink, w: u16) {
        self.put_byte(sink, (w >> 8) as u8);
        self.put_byte(sink, w as u8);
    }

    fn put_marker(&mut self, sink: &mut dyn Sink, m: u8) {
        self.put_byte(sink, 0xFF);
        self.put_byte(sink, m);
    }

    #[inline(always)]
    fn put_bits(&mut self, sink: &mut dyn Sink, value: u32, nbits: u32) {
        self.bitbuf = (self.bitbuf << nbits) | (value & ((1 << nbits) - 1));
        self.bitcnt += nbits;
        while self.bitcnt >= 8 {
            self.bitcnt -= 8;
            let b = (self.bitbuf >> self.bitcnt) as u8;
            self.put_byte(sink, b);
            if b == 0xFF {
                self.put_byte(sink, 0);
            }
        }
    }

    fn flush_bits(&mut self, sink: &mut dyn Sink) {
        if self.bitcnt > 0 {
            let pad = 8 - self.bitcnt;
            self.put_bits(sink, (1 << pad) - 1, pad);
        }
    }

    fn write_dqt(&mut self, sink: &mut dyn Sink, id: usize) {
        self.put_marker(sink, 0xDB);
        self.put_word(sink, 3 + 64);
        self.put_byte(sink, id as u8);
        for i in 0..64 {
            let v = self.t.qt_byte[id][usize::from(ZIGZAG[i])];
            self.put_byte(sink, v);
        }
    }

    fn write_dht(&mut self, sink: &mut dyn Sink, tc: u8, th: u8, bits: &[u8; 17], vals: &[u8]) {
        let total: usize = bits[1..].iter().map(|&b| usize::from(b)).sum();
        self.put_marker(sink, 0xC4);
        self.put_word(sink, (3 + 16 + total) as u16);
        self.put_byte(sink, (tc << 4) | th);
        for &b in &bits[1..] {
            self.put_byte(sink, b);
        }
        for &v in &vals[..total] {
            self.put_byte(sink, v);
        }
    }

    fn write_headers(&mut self, sink: &mut dyn Sink) {
        let n = self.comps as u8;
        self.put_marker(sink, 0xD8);
        self.write_dqt(sink, 0);
        if n == 3 {
            self.write_dqt(sink, 1);
        }

        self.put_marker(sink, 0xC0);
        self.put_word(sink, 8 + 3 * u16::from(n));
        self.put_byte(sink, 8);
        self.put_word(sink, self.height as u16);
        self.put_word(sink, self.width as u16);
        self.put_byte(sink, n);
        for c in 0..n {
            self.put_byte(sink, c + 1);
            let sampling = if c == 0 { ((self.sub_h as u8) << 4) | self.sub_v as u8 } else { 0x11 };
            self.put_byte(sink, sampling);
            self.put_byte(sink, u8::from(c != 0));
        }

        self.write_dht(sink, 0, 0, &DC_L_BITS, &DC_VALS);
        self.write_dht(sink, 1, 0, &AC_L_BITS, &AC_L_VALS);
        if n == 3 {
            self.write_dht(sink, 0, 1, &DC_C_BITS, &DC_VALS);
            self.write_dht(sink, 1, 1, &AC_C_BITS, &AC_C_VALS);
        }

        self.put_marker(sink, 0xDA);
        self.put_word(sink, 6 + 2 * u16::from(n));
        self.put_byte(sink, n);
        for c in 0..n {
            self.put_byte(sink, c + 1);
            self.put_byte(sink, if c == 0 { 0x00 } else { 0x11 });
        }
        self.put_byte(sink, 0);
        self.put_byte(sink, 63);
        self.put_byte(sink, 0);
    }

    fn encode_block(&mut self, sink: &mut dyn Sink, block: &mut [i32; 64], comp: usize) {
        let id = usize::from(comp != 0);
        fdct_8x8(block);

        let mut zz = [0i32; 64];
        {
            let qaan = &self.t.qt_aan[id];
            for k in 0..64 {
                let nat = usize::from(ZIGZAG[k]);
                zz[k] = sdiv(block[nat], qaan[nat]);
            }
        }

        let diff = zz[0] - self.dc_pred[comp];
        self.dc_pred[comp] = zz[0];
        let n = magnitude(diff);
        let (code, len) = (self.t.hdc_code[id][n as usize & 15], self.t.hdc_len[id][n as usize & 15]);
        self.put_bits(sink, u32::from(code), u32::from(len));
        if n != 0 {
            self.put_bits(sink, extend_encode(diff, n), n);
        }

        let mut last_nz = 63;
        while last_nz > 0 && zz[last_nz] == 0 {
            last_nz -= 1;
        }
        let mut run = 0;
        for &v in &zz[1..=last_nz] {
            if v == 0 {
                run += 1;
                continue;
            }
            while run >= 16 {
                let (code, len) = (self.t.hac_code[id][0xF0], self.t.hac_len[id][0xF0]);
                self.put_bits(sink, u32::from(code), u32::from(len));
                run -= 16;
            }
            let nb = magnitude(v);
            let sym = ((run << 4) | nb) as usize & 0xFF;
            let (code, len) = (self.t.hac_code[id][sym], self.t.hac_len[id][sym]);
            self.put_bits(sink, u32::from(code), u32::from(len));
            self.put_bits(sink, extend_encode(v, nb), nb);
            run = 0;
        }
        if last_nz < 63 {
            let (code, len) = (self.t.hac_code[id][0], self.t.hac_len[id][0]);
            self.put_bits(sink, u32::from(code), u32::from(len));
        }
    }

    fn extract_y_block(&self, x0: usize, by: usize, bx: usize, out: &mut [i32; 64]) {
        let n = self.comps;
        let stride = self.width_pad * n;
        for y in 0..8 {
            let row = &self.blocks[(by * 8 + y) * stride + (x0 + bx * 8) * n..];
            for x in 0..8 {
                out[y * 8 + x] = i32::from(row[x * n]);
            }
        }
    }

    fn extract_chroma_block(&self, x0: usize, comp: usize, out: &mut [i32; 64]) {
        let stride = self.width_pad * 3;
        let (sh, sv) = (self.sub_h, self.sub_v);
        let area = (sh * sv) as i32;
        let half = area / 2;
        for y in 0..8 {
            for x in 0..8 {
                let mut sum = 0;
                for vy in 0..sv {
                    let row = &self.blocks[(y * sv + vy) * stride + (x0 + x * sh) * 3 + comp..];
                    for hx in 0..sh {
                        sum += i32::from(row[hx * 3]);
                    }
                }
                out[y * 8 + x] = (if sum >= 0 { sum + half } else { sum - half }) / area;
            }
        }
    }

    fn encode_mcu_row(&mut self, sink: &mut dyn Sink) {
        let mut block = [0; 64];
        for mx in 0..self.width_pad / self.mcu_w {
            let x0 = mx * self.mcu_w;
            for by in 0..self.sub_v {
                for bx in 0..self.sub_h {
                    self.extract_y_block(x0, by, bx, &mut block);
                    self.encode_block(sink, &mut block, 0);
                }
            }
            if self.comps == 3 {
                for comp in 1..=2 {
                    self.extract_chroma_block(x0, comp, &mut block);
                    self.encode_block(sink, &mut block, comp);
                }
            }
        }
    }

    fn absorb_row(&mut self, src: &[u8]) {
        let (w, pad, n) = (self.width, self.width_pad, self.comps);
        let dst = &mut self.blocks[self.rows_in_buf * pad * n..][..pad * n];
        if n == 1 {
            for (d, &s) in dst.iter_mut().zip(&src[..w]) {
                *d = i16::from(s) - 128;
            }
            let last = dst[w - 1];
            dst[w..].fill(last);
        } else {
            for x in 0..w {
                let (r, g, b);
                if self.input_565 {
                    let v = u16::from_ne_bytes([src[2 * x], src[2 * x + 1]]);
                    let (r5, g6, b5) = (i32::from(v >> 11) & 0x1F, i32::from(v >> 5) & 0x3F, i32::from(v) & 0x1F);
                    r = (r5 << 3) | (r5 >> 2);
                    g = (g6 << 2) | (g6 >> 4);
                    b = (b5 << 3) | (b5 >> 2);
                } else {
                    let p = &src[3 * x..3 * x + 3];
                    (r, g, b) = if self.input_bgr {
                        (i32::from(p[2]), i32::from(p[1]), i32::from(p[0]))
                    } else {
                        (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]))
                    };
                }
                let yv = (77 * r + 150 * g + 29 * b + 128) >> 8;
                let cbv = (-43 * r - 85 * g + 128 * b + 32896) >> 8;
                let crv = (128 * r - 107 * g - 21 * b + 32896) >> 8;
                dst[3 * x] = (yv - 128) as i16;
                dst[3 * x + 1] = (cbv - 128) as i16;
                dst[3 * x + 2] = (crv - 128) as i16;
            }
            for x in w..pad {
                dst.copy_within(3 * (w - 1)..3 * w, 3 * x);
            }
        }
        self.rows_in_buf += 1;
    }

    fn pad_last_rows(&mut self) {
        if self.rows_in_buf == 0 || self.rows_in_buf >= self.mcu_h {
            return;
        }
        let row = self.width_pad * self.comps;
        let last = (self.rows_in_buf - 1) * row;
        while self.rows_in_buf < self.mcu_h {
            self.blocks.copy_within(last..last + row, self.rows_in_buf * row);
            self.rows_in_buf += 1;
        }
    }

    fn push_row(&mut self, sink: &mut dyn Sink, row: &[u8]) -> Result<()> {
        if self.total_rows >= self.height {
            return Err(Error::InvalidState);
        }
        self.absorb_row(row);
        self.total_rows += 1;
        if self.rows_in_buf == self.mcu_h {
            self.encode_mcu_row(sink);
            self.rows_in_buf = 0;
        }
        self.failed.map_or(Ok(()), Err)
    }

    fn finish(&mut self, sink: &mut dyn Sink) -> Result<usize> {
        if self.total_rows != self.height {
            return Err(Error::InvalidState);
        }
        if self.rows_in_buf > 0 {
            self.pad_last_rows();
            self.encode_mcu_row(sink);
            self.rows_in_buf = 0;
        }
        self.flush_bits(sink);
        self.put_marker(sink, 0xD9);
        self.drain(sink);
        self.failed.map_or(Ok(self.total), Err)
    }
}

/// Baseline JPEG with the Annex K tables. A sink's error is sticky.
pub struct JpegEncoder<S: Sink> {
    core: Core,
    sink: S,
    format: PixelFormat,
}

impl<S: Sink> JpegEncoder<S> {
    /// Writes the headers to `sink`.
    pub fn new(width: u16, height: u16, format: PixelFormat, options: &JpegOptions, mut sink: S) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidArgument);
        }
        let quality = if options.quality == 0 { 75 } else { i32::from(options.quality.min(100)) };
        let comps = if format == PixelFormat::Gray8 { 1 } else { 3 };
        let (sub_h, sub_v) = match (comps, options.subsampling) {
            (1, _) | (_, Subsampling::S444) => (1, 1),
            (_, Subsampling::S422) => (2, 1),
            _ => (2, 2),
        };
        let (mcu_w, mcu_h) = (8 * sub_h, 8 * sub_v);
        let width_pad = usize::from(width).next_multiple_of(mcu_w);

        let mut t: Box<Tables, Caps> = state()?;
        scale_table(&STD_Q_L, &mut t.qt_byte[0], quality);
        scale_table(&STD_Q_C, &mut t.qt_byte[1], quality);
        for id in 0..2 {
            for i in 0..64 {
                let q = f64::from(t.qt_byte[id][i]) * AAN_SCALE[i >> 3] * AAN_SCALE[i & 7] * 8.0;
                t.qt_aan[id][i] = lround(q).max(1);
            }
        }
        let tables = &mut *t;
        build_huffman_codes(&DC_L_BITS, &DC_VALS, &mut tables.hdc_code[0], &mut tables.hdc_len[0]);
        build_huffman_codes(&DC_C_BITS, &DC_VALS, &mut tables.hdc_code[1], &mut tables.hdc_len[1]);
        build_huffman_codes(&AC_L_BITS, &AC_L_VALS, &mut tables.hac_code[0], &mut tables.hac_len[0]);
        build_huffman_codes(&AC_C_BITS, &AC_C_VALS, &mut tables.hac_code[1], &mut tables.hac_len[1]);

        let mut core = Core {
            width: usize::from(width),
            height: usize::from(height),
            comps,
            sub_h,
            sub_v,
            mcu_w,
            mcu_h,
            width_pad,
            input_565: format == PixelFormat::Rgb565,
            input_bgr: format == PixelFormat::Bgr888,
            t,
            blocks: buffer(mcu_h * width_pad * comps, options.caps)?,
            rows_in_buf: 0,
            total_rows: 0,
            dc_pred: [0; 3],
            bitbuf: 0,
            bitcnt: 0,
            staging: internal_buffer(STAGING_BYTES)?,
            staged: 0,
            total: 0,
            failed: None,
        };
        core.write_headers(&mut sink);
        core.failed.map_or(Ok(()), Err)?;
        Ok(Self { core, sink, format })
    }
}

impl<S: Sink> Encoder for JpegEncoder<S> {
    /// Bytes written, and the sink back.
    type Output = (usize, S);

    fn info(&self) -> ImageInfo {
        ImageInfo { width: self.core.width as u16, height: self.core.height as u16, format: self.format }
    }

    fn push_row(&mut self, row: &[u8]) -> Result<()> {
        if row.len() < self.info().row_bytes() {
            return Err(Error::InvalidArgument);
        }
        self.core.push_row(&mut self.sink, row)
    }

    fn finish(mut self) -> Result<(usize, S)> {
        let written = self.core.finish(&mut self.sink)?;
        Ok((written, self.sink))
    }
}
