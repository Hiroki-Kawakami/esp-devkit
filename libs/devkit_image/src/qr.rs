/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::heap::Caps;

use crate::mem::{Buffer, buffer, small_buffer};
use crate::{Error, ImageBuf, ImageFormat, Result, default_caps};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ecc {
    L,
    M,
    Q,
    H,
}

impl Ecc {
    const fn format_bits(self) -> u32 {
        match self {
            Self::L => 1,
            Self::M => 0,
            Self::Q => 3,
            Self::H => 2,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct QrOptions {
    pub ecc: Ecc,
    /// Pixels per module.
    pub scale: u16,
    /// Quiet zone, in modules.
    pub border: u16,
    pub caps: Option<Caps>,
}

impl Default for QrOptions {
    fn default() -> Self {
        Self { ecc: Ecc::M, scale: 4, border: 4, caps: None }
    }
}

/// `data` as an `A8` QR code: dark modules opaque, light ones and the border transparent.
pub fn qr_image(data: &[u8], options: &QrOptions) -> Result<ImageBuf<Caps>> {
    if options.scale == 0 {
        return Err(Error::InvalidArgument);
    }
    let qr = QrCode::encode(data, options.ecc, None)?;
    let side = (qr.size + 2 * usize::from(options.border)) * usize::from(options.scale);
    let side = u16::try_from(side).map_err(|_| Error::TooLarge)?;
    let caps = options.caps.unwrap_or_else(default_caps);
    let mut image = ImageBuf::new_in(ImageFormat::A8, side, side, usize::from(side), 1, caps)?;
    let width = usize::from(side);
    let scale = usize::from(options.scale);
    let origin = usize::from(options.border) * scale;
    let pixels = image.pixels_mut();
    for y in 0..qr.size {
        let top = (origin + y * scale) * width;
        let row = &mut pixels[top..top + width];
        for x in 0..qr.size {
            if qr.dark(x, y) {
                let left = origin + x * scale;
                row[left..left + scale].fill(0xff);
            }
        }
        for i in 1..scale {
            pixels.copy_within(top..top + width, top + i * width);
        }
    }
    Ok(image)
}

const DARK: u8 = 1;
const FUNCTION: u8 = 2;

pub(crate) struct QrCode {
    pub(crate) version: usize,
    pub(crate) size: usize,
    pub(crate) mask: u8,
    modules: Buffer,
}

impl QrCode {
    /// The smallest version that holds `data` in one segment of the densest mode it fits.
    /// `mask` of `None` picks the one with the lowest penalty.
    pub(crate) fn encode(data: &[u8], ecc: Ecc, mask: Option<u8>) -> Result<Self> {
        let mode = Mode::of(data);
        let version = (1..=40)
            .find(|&v| {
                let count_bits = mode.count_bits(v);
                data.len() < 1 << count_bits && 4 + count_bits + mode.data_bits(data.len()) <= data_codewords(v, ecc) * 8
            })
            .ok_or(Error::TooLarge)?;

        let mut payload = small_buffer::<u8>(data_codewords(version, ecc))?;
        let mut bits = BitWriter { buf: &mut payload, len: 0 };
        bits.push(mode.indicator(), 4);
        bits.push(data.len() as u32, mode.count_bits(version));
        mode.write(data, &mut bits);
        let capacity = bits.buf.len() * 8;
        let used = (bits.len + (capacity - bits.len).min(4)).next_multiple_of(8);
        for (i, b) in payload[used / 8..].iter_mut().enumerate() {
            *b = if i % 2 == 0 { 0xec } else { 0x11 };
        }

        let codewords = interleave(&payload, version, ecc)?;
        let size = version * 4 + 17;
        let mut qr = Self { version, size, mask: 0, modules: buffer(size * size, None)? };
        qr.draw_function_patterns(ecc);
        qr.place(&codewords);

        let mask = match mask {
            Some(m) if m < 8 => m,
            Some(_) => return Err(Error::InvalidArgument),
            None => {
                let mut best = (u32::MAX, 0);
                for m in 0..8 {
                    qr.apply_mask(m);
                    qr.draw_format(ecc, m);
                    let p = qr.penalty();
                    if p < best.0 {
                        best = (p, m);
                    }
                    qr.apply_mask(m);
                }
                best.1
            }
        };
        qr.apply_mask(mask);
        qr.draw_format(ecc, mask);
        qr.mask = mask;
        Ok(qr)
    }

    pub(crate) fn dark(&self, x: usize, y: usize) -> bool {
        self.modules[y * self.size + x] & DARK != 0
    }

    fn function(&self, x: usize, y: usize) -> bool {
        self.modules[y * self.size + x] & FUNCTION != 0
    }

    fn set_function(&mut self, x: usize, y: usize, dark: bool) {
        self.modules[y * self.size + x] = FUNCTION | u8::from(dark);
    }

    fn draw_function_patterns(&mut self, ecc: Ecc) {
        let size = self.size;
        for i in 0..size {
            self.set_function(6, i, i % 2 == 0);
            self.set_function(i, 6, i % 2 == 0);
        }
        for (cx, cy) in [(3, 3), (size - 4, 3), (3, size - 4)] {
            for dy in -4isize..=4 {
                for dx in -4isize..=4 {
                    let (x, y) = (cx as isize + dx, cy as isize + dy);
                    if (0..size as isize).contains(&x) && (0..size as isize).contains(&y) {
                        let dist = dx.abs().max(dy.abs());
                        self.set_function(x as usize, y as usize, dist != 2 && dist != 4);
                    }
                }
            }
        }
        let (positions, count) = alignment_positions(self.version);
        for i in 0..count {
            for j in 0..count {
                if (i == 0 && j == 0) || (i == 0 && j == count - 1) || (i == count - 1 && j == 0) {
                    continue;
                }
                for dy in -2isize..=2 {
                    for dx in -2isize..=2 {
                        let (x, y) = ((positions[i] as isize + dx) as usize, (positions[j] as isize + dy) as usize);
                        self.set_function(x, y, dx.abs().max(dy.abs()) != 1);
                    }
                }
            }
        }
        self.draw_format(ecc, 0);
        if self.version >= 7 {
            let mut rem = self.version as u32;
            for _ in 0..12 {
                rem = (rem << 1) ^ ((rem >> 11) * 0x1f25);
            }
            let bits = (self.version as u32) << 12 | rem;
            for i in 0..18 {
                let dark = bits >> i & 1 != 0;
                let (a, b) = (size - 11 + i % 3, i / 3);
                self.set_function(a, b, dark);
                self.set_function(b, a, dark);
            }
        }
    }

    fn draw_format(&mut self, ecc: Ecc, mask: u8) {
        let data = ecc.format_bits() << 3 | u32::from(mask);
        let mut rem = data;
        for _ in 0..10 {
            rem = (rem << 1) ^ ((rem >> 9) * 0x537);
        }
        let bits = (data << 10 | rem) ^ 0x5412;
        let bit = |i: usize| bits >> i & 1 != 0;
        let size = self.size;
        for i in 0..6 {
            self.set_function(8, i, bit(i));
        }
        self.set_function(8, 7, bit(6));
        self.set_function(8, 8, bit(7));
        self.set_function(7, 8, bit(8));
        for i in 9..15 {
            self.set_function(14 - i, 8, bit(i));
        }
        for i in 0..8 {
            self.set_function(size - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set_function(8, size - 15 + i, bit(i));
        }
        self.set_function(8, size - 8, true);
    }

    fn place(&mut self, codewords: &[u8]) {
        let size = self.size;
        let total = codewords.len() * 8;
        let mut i = 0;
        let mut right = size - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            let upward = (right + 1) & 2 == 0;
            for vert in 0..size {
                let y = if upward { size - 1 - vert } else { vert };
                for x in [right, right - 1] {
                    if !self.function(x, y) && i < total {
                        self.modules[y * size + x] = codewords[i >> 3] >> (7 - (i & 7)) & 1;
                        i += 1;
                    }
                }
            }
            if right < 2 {
                break;
            }
            right -= 2;
        }
    }

    fn apply_mask(&mut self, mask: u8) {
        for y in 0..self.size {
            for x in 0..self.size {
                let invert = match mask {
                    0 => (x + y) % 2 == 0,
                    1 => y % 2 == 0,
                    2 => x % 3 == 0,
                    3 => (x + y) % 3 == 0,
                    4 => (x / 3 + y / 2) % 2 == 0,
                    5 => x * y % 2 + x * y % 3 == 0,
                    6 => (x * y % 2 + x * y % 3) % 2 == 0,
                    _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
                };
                let m = &mut self.modules[y * self.size + x];
                if invert && *m & FUNCTION == 0 {
                    *m ^= DARK;
                }
            }
        }
    }

    fn penalty(&self) -> u32 {
        let size = self.size;
        let mut penalty = 0;
        for transpose in [false, true] {
            for a in 0..size {
                let mut runs = FinderRuns::new(size);
                let mut color = false;
                let mut run = 0;
                for b in 0..size {
                    let dark = if transpose { self.dark(a, b) } else { self.dark(b, a) };
                    if dark == color {
                        run += 1;
                        if run == 5 {
                            penalty += 3;
                        } else if run > 5 {
                            penalty += 1;
                        }
                    } else {
                        runs.push(run);
                        if !color {
                            penalty += runs.count() * 40;
                        }
                        color = dark;
                        run = 1;
                    }
                }
                penalty += runs.finish(color, run) * 40;
            }
        }
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let c = self.dark(x, y);
                if c == self.dark(x + 1, y) && c == self.dark(x, y + 1) && c == self.dark(x + 1, y + 1) {
                    penalty += 3;
                }
            }
        }
        let dark = self.modules.iter().filter(|&&m| m & DARK != 0).count();
        let total = size * size;
        let k = (dark * 20).abs_diff(total * 10).div_ceil(total) - 1;
        penalty + k as u32 * 10
    }
}

/// The last seven runs of a row, newest first, for the 1:1:3:1:1 finder-like pattern.
struct FinderRuns {
    size: usize,
    runs: [usize; 7],
}

impl FinderRuns {
    fn new(size: usize) -> Self {
        Self { size, runs: [0; 7] }
    }

    fn push(&mut self, mut run: usize) {
        if self.runs[0] == 0 {
            run += self.size;
        }
        self.runs.copy_within(0..6, 1);
        self.runs[0] = run;
    }

    fn count(&self) -> u32 {
        let r = &self.runs;
        let n = r[1];
        let core = n > 0 && r[2] == n && r[3] == n * 3 && r[4] == n && r[5] == n;
        u32::from(core && r[0] >= n * 4 && r[6] >= n) + u32::from(core && r[6] >= n * 4 && r[0] >= n)
    }

    fn finish(&mut self, color: bool, mut run: usize) -> u32 {
        if color {
            self.push(run);
            run = 0;
        }
        self.push(run + self.size);
        self.count()
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Numeric,
    Alphanumeric,
    Byte,
}

const ALPHANUMERIC: &[u8; 45] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

impl Mode {
    fn of(data: &[u8]) -> Self {
        if data.iter().all(u8::is_ascii_digit) {
            Self::Numeric
        } else if data.iter().all(|b| ALPHANUMERIC.contains(b)) {
            Self::Alphanumeric
        } else {
            Self::Byte
        }
    }

    const fn indicator(self) -> u32 {
        match self {
            Self::Numeric => 1,
            Self::Alphanumeric => 2,
            Self::Byte => 4,
        }
    }

    const fn count_bits(self, version: usize) -> usize {
        let i = if version <= 9 { 0 } else if version <= 26 { 1 } else { 2 };
        match self {
            Self::Numeric => [10, 12, 14][i],
            Self::Alphanumeric => [9, 11, 13][i],
            Self::Byte => [8, 16, 16][i],
        }
    }

    const fn data_bits(self, len: usize) -> usize {
        match self {
            Self::Numeric => len / 3 * 10 + [0, 4, 7][len % 3],
            Self::Alphanumeric => len / 2 * 11 + len % 2 * 6,
            Self::Byte => len * 8,
        }
    }

    fn write(self, data: &[u8], bits: &mut BitWriter) {
        match self {
            Self::Numeric => {
                for chunk in data.chunks(3) {
                    let value = chunk.iter().fold(0, |v, &d| v * 10 + u32::from(d - b'0'));
                    bits.push(value, chunk.len() * 3 + 1);
                }
            }
            Self::Alphanumeric => {
                let index = |b: u8| ALPHANUMERIC.iter().position(|&c| c == b).unwrap() as u32;
                for chunk in data.chunks(2) {
                    match *chunk {
                        [a, b] => bits.push(index(a) * 45 + index(b), 11),
                        [a] => bits.push(index(a), 6),
                        _ => unreachable!(),
                    }
                }
            }
            Self::Byte => {
                for &b in data {
                    bits.push(b.into(), 8);
                }
            }
        }
    }
}

struct BitWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl BitWriter<'_> {
    fn push(&mut self, value: u32, count: usize) {
        for i in (0..count).rev() {
            if value >> i & 1 != 0 {
                self.buf[self.len >> 3] |= 0x80 >> (self.len & 7);
            }
            self.len += 1;
        }
    }
}

const ECC_PER_BLOCK: [[u8; 41]; 4] = [
    [0, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    [0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28],
    [0, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30, 30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    [0, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
];

const BLOCKS: [[u8; 41]; 4] = [
    [0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25],
    [0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49],
    [0, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68],
    [0, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32, 35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81],
];

fn raw_modules(version: usize) -> usize {
    let mut n = (16 * version + 128) * version + 64;
    if version >= 2 {
        let align = version / 7 + 2;
        n -= (25 * align - 10) * align - 55;
        if version >= 7 {
            n -= 36;
        }
    }
    n
}

fn data_codewords(version: usize, ecc: Ecc) -> usize {
    let e = ecc as usize;
    raw_modules(version) / 8 - usize::from(ECC_PER_BLOCK[e][version]) * usize::from(BLOCKS[e][version])
}

fn ecc_layout(version: usize, ecc: Ecc) -> (usize, usize) {
    let e = ecc as usize;
    (usize::from(BLOCKS[e][version]), usize::from(ECC_PER_BLOCK[e][version]))
}

fn alignment_positions(version: usize) -> ([usize; 7], usize) {
    let mut positions = [0; 7];
    if version == 1 {
        return (positions, 0);
    }
    let count = version / 7 + 2;
    let step = if version == 32 { 26 } else { (version * 4 + count * 2 + 1) / (count * 2 - 2) * 2 };
    let last = version * 4 + 10;
    positions[0] = 6;
    for (i, p) in positions[1..count].iter_mut().enumerate() {
        *p = last - (count - 2 - i) * step;
    }
    (positions, count)
}

fn interleave(data: &[u8], version: usize, ecc: Ecc) -> Result<Buffer> {
    let (blocks, ecc_len) = ecc_layout(version, ecc);
    let raw = raw_modules(version) / 8;
    let short_blocks = blocks - raw % blocks;
    let short_data = raw / blocks - ecc_len;
    let mut divisor = [0u8; 30];
    rs_divisor(&mut divisor[..ecc_len]);
    let mut out = small_buffer::<u8>(raw)?;
    let mut offset = 0;
    for j in 0..blocks {
        let len = short_data + usize::from(j >= short_blocks);
        let block = &data[offset..offset + len];
        offset += len;
        for (i, &b) in block.iter().enumerate() {
            let at = if i < short_data { i * blocks + j } else { short_data * blocks + j - short_blocks };
            out[at] = b;
        }
        let mut rem = [0u8; 30];
        rs_remainder(block, &divisor[..ecc_len], &mut rem[..ecc_len]);
        for (i, &b) in rem[..ecc_len].iter().enumerate() {
            out[data.len() + i * blocks + j] = b;
        }
    }
    Ok(out)
}

fn gf_mul(x: u8, y: u8) -> u8 {
    let mut z = 0u32;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x11d);
        z ^= u32::from(y >> i & 1) * u32::from(x);
    }
    z as u8
}

/// Coefficients of the generator polynomial, highest degree first, the leading 1 left out.
pub(crate) fn rs_divisor(out: &mut [u8]) {
    let n = out.len();
    out.fill(0);
    out[n - 1] = 1;
    let mut root = 1;
    for _ in 0..n {
        for j in 0..n {
            out[j] = gf_mul(out[j], root);
            if j + 1 < n {
                out[j] ^= out[j + 1];
            }
        }
        root = gf_mul(root, 2);
    }
}

pub(crate) fn rs_remainder(data: &[u8], divisor: &[u8], out: &mut [u8]) {
    let n = divisor.len();
    out.fill(0);
    for &b in data {
        let factor = b ^ out[0];
        out.copy_within(1..n, 0);
        out[n - 1] = 0;
        for (r, &d) in out.iter_mut().zip(divisor) {
            *r ^= gf_mul(d, factor);
        }
    }
}
