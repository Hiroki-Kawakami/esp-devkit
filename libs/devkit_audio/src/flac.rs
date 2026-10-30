/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::heap::{self, Caps};

use crate::{Decoder, Error, Format, PcmMut, Result};

pub const FLAC_STREAMINFO_BYTES: usize = 34;

const MAX_BPS: u8 = 24;
const MAX_ORDER: usize = 32;

const CRC8: [u8; 256] = [
    0x00, 0x07, 0x0e, 0x09, 0x1c, 0x1b, 0x12, 0x15, 0x38, 0x3f, 0x36, 0x31,
    0x24, 0x23, 0x2a, 0x2d, 0x70, 0x77, 0x7e, 0x79, 0x6c, 0x6b, 0x62, 0x65,
    0x48, 0x4f, 0x46, 0x41, 0x54, 0x53, 0x5a, 0x5d, 0xe0, 0xe7, 0xee, 0xe9,
    0xfc, 0xfb, 0xf2, 0xf5, 0xd8, 0xdf, 0xd6, 0xd1, 0xc4, 0xc3, 0xca, 0xcd,
    0x90, 0x97, 0x9e, 0x99, 0x8c, 0x8b, 0x82, 0x85, 0xa8, 0xaf, 0xa6, 0xa1,
    0xb4, 0xb3, 0xba, 0xbd, 0xc7, 0xc0, 0xc9, 0xce, 0xdb, 0xdc, 0xd5, 0xd2,
    0xff, 0xf8, 0xf1, 0xf6, 0xe3, 0xe4, 0xed, 0xea, 0xb7, 0xb0, 0xb9, 0xbe,
    0xab, 0xac, 0xa5, 0xa2, 0x8f, 0x88, 0x81, 0x86, 0x93, 0x94, 0x9d, 0x9a,
    0x27, 0x20, 0x29, 0x2e, 0x3b, 0x3c, 0x35, 0x32, 0x1f, 0x18, 0x11, 0x16,
    0x03, 0x04, 0x0d, 0x0a, 0x57, 0x50, 0x59, 0x5e, 0x4b, 0x4c, 0x45, 0x42,
    0x6f, 0x68, 0x61, 0x66, 0x73, 0x74, 0x7d, 0x7a, 0x89, 0x8e, 0x87, 0x80,
    0x95, 0x92, 0x9b, 0x9c, 0xb1, 0xb6, 0xbf, 0xb8, 0xad, 0xaa, 0xa3, 0xa4,
    0xf9, 0xfe, 0xf7, 0xf0, 0xe5, 0xe2, 0xeb, 0xec, 0xc1, 0xc6, 0xcf, 0xc8,
    0xdd, 0xda, 0xd3, 0xd4, 0x69, 0x6e, 0x67, 0x60, 0x75, 0x72, 0x7b, 0x7c,
    0x51, 0x56, 0x5f, 0x58, 0x4d, 0x4a, 0x43, 0x44, 0x19, 0x1e, 0x17, 0x10,
    0x05, 0x02, 0x0b, 0x0c, 0x21, 0x26, 0x2f, 0x28, 0x3d, 0x3a, 0x33, 0x34,
    0x4e, 0x49, 0x40, 0x47, 0x52, 0x55, 0x5c, 0x5b, 0x76, 0x71, 0x78, 0x7f,
    0x6a, 0x6d, 0x64, 0x63, 0x3e, 0x39, 0x30, 0x37, 0x22, 0x25, 0x2c, 0x2b,
    0x06, 0x01, 0x08, 0x0f, 0x1a, 0x1d, 0x14, 0x13, 0xae, 0xa9, 0xa0, 0xa7,
    0xb2, 0xb5, 0xbc, 0xbb, 0x96, 0x91, 0x98, 0x9f, 0x8a, 0x8d, 0x84, 0x83,
    0xde, 0xd9, 0xd0, 0xd7, 0xc2, 0xc5, 0xcc, 0xcb, 0xe6, 0xe1, 0xe8, 0xef,
    0xfa, 0xfd, 0xf4, 0xf3,
];

const CRC16: [u16; 256] = [
    0x0000, 0x8005, 0x800f, 0x000a, 0x801b, 0x001e, 0x0014, 0x8011, 0x8033, 0x0036,
    0x003c, 0x8039, 0x0028, 0x802d, 0x8027, 0x0022, 0x8063, 0x0066, 0x006c, 0x8069,
    0x0078, 0x807d, 0x8077, 0x0072, 0x0050, 0x8055, 0x805f, 0x005a, 0x804b, 0x004e,
    0x0044, 0x8041, 0x80c3, 0x00c6, 0x00cc, 0x80c9, 0x00d8, 0x80dd, 0x80d7, 0x00d2,
    0x00f0, 0x80f5, 0x80ff, 0x00fa, 0x80eb, 0x00ee, 0x00e4, 0x80e1, 0x00a0, 0x80a5,
    0x80af, 0x00aa, 0x80bb, 0x00be, 0x00b4, 0x80b1, 0x8093, 0x0096, 0x009c, 0x8099,
    0x0088, 0x808d, 0x8087, 0x0082, 0x8183, 0x0186, 0x018c, 0x8189, 0x0198, 0x819d,
    0x8197, 0x0192, 0x01b0, 0x81b5, 0x81bf, 0x01ba, 0x81ab, 0x01ae, 0x01a4, 0x81a1,
    0x01e0, 0x81e5, 0x81ef, 0x01ea, 0x81fb, 0x01fe, 0x01f4, 0x81f1, 0x81d3, 0x01d6,
    0x01dc, 0x81d9, 0x01c8, 0x81cd, 0x81c7, 0x01c2, 0x0140, 0x8145, 0x814f, 0x014a,
    0x815b, 0x015e, 0x0154, 0x8151, 0x8173, 0x0176, 0x017c, 0x8179, 0x0168, 0x816d,
    0x8167, 0x0162, 0x8123, 0x0126, 0x012c, 0x8129, 0x0138, 0x813d, 0x8137, 0x0132,
    0x0110, 0x8115, 0x811f, 0x011a, 0x810b, 0x010e, 0x0104, 0x8101, 0x8303, 0x0306,
    0x030c, 0x8309, 0x0318, 0x831d, 0x8317, 0x0312, 0x0330, 0x8335, 0x833f, 0x033a,
    0x832b, 0x032e, 0x0324, 0x8321, 0x0360, 0x8365, 0x836f, 0x036a, 0x837b, 0x037e,
    0x0374, 0x8371, 0x8353, 0x0356, 0x035c, 0x8359, 0x0348, 0x834d, 0x8347, 0x0342,
    0x03c0, 0x83c5, 0x83cf, 0x03ca, 0x83db, 0x03de, 0x03d4, 0x83d1, 0x83f3, 0x03f6,
    0x03fc, 0x83f9, 0x03e8, 0x83ed, 0x83e7, 0x03e2, 0x83a3, 0x03a6, 0x03ac, 0x83a9,
    0x03b8, 0x83bd, 0x83b7, 0x03b2, 0x0390, 0x8395, 0x839f, 0x039a, 0x838b, 0x038e,
    0x0384, 0x8381, 0x0280, 0x8285, 0x828f, 0x028a, 0x829b, 0x029e, 0x0294, 0x8291,
    0x82b3, 0x02b6, 0x02bc, 0x82b9, 0x02a8, 0x82ad, 0x82a7, 0x02a2, 0x82e3, 0x02e6,
    0x02ec, 0x82e9, 0x02f8, 0x82fd, 0x82f7, 0x02f2, 0x02d0, 0x82d5, 0x82df, 0x02da,
    0x82cb, 0x02ce, 0x02c4, 0x82c1, 0x8243, 0x0246, 0x024c, 0x8249, 0x0258, 0x825d,
    0x8257, 0x0252, 0x0270, 0x8275, 0x827f, 0x027a, 0x826b, 0x026e, 0x0264, 0x8261,
    0x0220, 0x8225, 0x822f, 0x022a, 0x823b, 0x023e, 0x0234, 0x8231, 0x8213, 0x0216,
    0x021c, 0x8219, 0x0208, 0x820d, 0x8207, 0x0202,
];

const RATES: [u32; 12] = [0, 88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000];
const SIZES: [u8; 8] = [0, 8, 12, 0, 16, 20, 24, 32];

#[derive(Clone, Copy, Debug)]
pub struct FlacConfig<'a> {
    /// The STREAMINFO block body, without its block header.
    pub streaminfo: &'a [u8],
    pub caps: Caps,
}

/// FLAC up to 24 bits and 8 channels. 16 bits or fewer decode to S16, more to
/// S32, both scaled to full scale.
pub struct FlacDecoder {
    format: Format,
    channels: u8,
    max_frames: usize,
    rate: u32,
    bps: u8,
    samples: alloc::boxed::Box<[i32], Caps>,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    bits: u32,
    pad: usize,
}

impl Bits<'_> {
    fn new(data: &[u8]) -> Bits<'_> {
        Bits { data, pos: 0, cache: 0, bits: 0, pad: 0 }
    }

    #[inline]
    fn refill(&mut self) {
        while self.bits <= 56 {
            let byte = match self.data.get(self.pos) {
                Some(&byte) => {
                    self.pos += 1;
                    byte
                }
                None => {
                    self.pad += 1;
                    0
                }
            };
            self.cache |= u64::from(byte) << (56 - self.bits);
            self.bits += 8;
        }
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.bits < n {
            self.refill();
        }
        let v = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.bits -= n;
        v
    }

    #[inline(always)]
    fn read_signed(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let v = self.read(n) << (32 - n);
        (v as i32) >> (32 - n)
    }

    #[inline(always)]
    fn overrun(&self) -> bool {
        self.pad * 8 > self.bits as usize
    }

    #[inline(always)]
    fn read_unary(&mut self) -> u32 {
        let mut zeros = 0u32;
        loop {
            if self.bits < 64 {
                self.refill();
            }
            if self.cache != 0 {
                let z = self.cache.leading_zeros();
                self.cache = (self.cache << z) << 1;
                self.bits -= z + 1;
                return zeros.wrapping_add(z);
            }
            zeros = zeros.wrapping_add(self.bits);
            self.cache = 0;
            self.bits = 0;
            if self.overrun() {
                return zeros;
            }
        }
    }

    fn consumed(&self) -> usize {
        self.pos + self.pad - self.bits as usize / 8
    }
}

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0, |crc, &b| CRC8[usize::from(crc ^ b)])
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0, |crc, &b| (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ b)])
}

struct FrameHeader {
    block: u32,
    assignment: u8,
}

fn read_residual(b: &mut Bits<'_>, s: &mut [i32], block: u32, order: u32) -> Result<()> {
    let method = b.read(2);
    if method > 1 {
        return Err(Error::InvalidData);
    }
    let param_bits = if method != 0 { 5 } else { 4 };
    let escape = if method != 0 { 31 } else { 15 };
    let partition_order = b.read(4);
    let partitions = 1u32 << partition_order;
    let per = block >> partition_order;
    if per << partition_order != block || per < order {
        return Err(Error::InvalidData);
    }

    let mut out = &mut s[order as usize..block as usize];
    for part in 0..partitions {
        let n = (if part != 0 { per } else { per - order }) as usize;
        let k = b.read(param_bits);
        let (now, rest) = out.split_at_mut(n);
        if k == escape {
            let raw = b.read(5);
            for v in now {
                *v = b.read_signed(raw);
            }
        } else {
            for o in now {
                let v = b.read_unary() << k | b.read(k);
                *o = (v >> 1) as i32 ^ -((v & 1) as i32);
            }
        }
        if b.overrun() {
            return Err(Error::InvalidSize);
        }
        out = rest;
    }
    Ok(())
}

fn restore_fixed(s: &mut [i32], order: u32) {
    let block = s.len();
    let u = s.as_mut_ptr().cast::<u32>();
    macro_rules! u {
        ($i:expr) => {
            *u.add($i)
        };
    }
    unsafe {
        match order {
            1 => {
                for i in 1..block {
                    u!(i) = u!(i).wrapping_add(u!(i - 1));
                }
            }
            2 => {
                for i in 2..block {
                    let p = 2u32.wrapping_mul(u!(i - 1)).wrapping_sub(u!(i - 2));
                    u!(i) = u!(i).wrapping_add(p);
                }
            }
            3 => {
                for i in 3..block {
                    let p = 3u32.wrapping_mul(u!(i - 1).wrapping_sub(u!(i - 2))).wrapping_add(u!(i - 3));
                    u!(i) = u!(i).wrapping_add(p);
                }
            }
            4 => {
                for i in 4..block {
                    let p = 4u32
                        .wrapping_mul(u!(i - 1).wrapping_add(u!(i - 3)))
                        .wrapping_sub(6u32.wrapping_mul(u!(i - 2)))
                        .wrapping_sub(u!(i - 4));
                    u!(i) = u!(i).wrapping_add(p);
                }
            }
            _ => {}
        }
    }
}

fn restore_lpc(s: &mut [i32], coef: &[i32], shift: u32, wide: bool) {
    let block = s.len();
    let order = coef.len();
    let p = s.as_mut_ptr();
    if wide {
        for i in order..block {
            let mut sum = 0i64;
            for (j, &c) in coef.iter().enumerate() {
                sum += i64::from(c) * i64::from(unsafe { *p.add(i - 1 - j) });
            }
            unsafe { *p.add(i) = (*p.add(i)).wrapping_add((sum >> shift) as i32) };
        }
        return;
    }
    for i in order..block {
        let x = unsafe { p.add(i - 1) };
        let mut sum = 0u32;
        for (j, &c) in coef.iter().enumerate() {
            sum = sum.wrapping_add((c as u32).wrapping_mul(unsafe { *x.sub(j) } as u32));
        }
        unsafe { *p.add(i) = (*p.add(i)).wrapping_add((sum as i32) >> shift) };
    }
}

fn bit_length(v: u32) -> u32 {
    32 - v.leading_zeros()
}

fn read_subframe(b: &mut Bits<'_>, s: &mut [i32], block: u32, bps: u32) -> Result<()> {
    if b.read(1) != 0 {
        return Err(Error::InvalidData);
    }
    let kind = b.read(6);
    let mut wasted = 0;
    if b.read(1) != 0 {
        wasted = b.read_unary().wrapping_add(1);
    }
    if wasted >= bps {
        return Err(Error::InvalidData);
    }
    let bps = bps - wasted;
    let s = &mut s[..block as usize];

    match kind {
        0 => s.fill(b.read_signed(bps)),
        1 => {
            for v in s.iter_mut() {
                *v = b.read_signed(bps);
            }
        }
        8..=12 => {
            let order = kind - 8;
            if order > block {
                return Err(Error::InvalidData);
            }
            for v in &mut s[..order as usize] {
                *v = b.read_signed(bps);
            }
            read_residual(b, s, block, order)?;
            restore_fixed(s, order);
        }
        32.. => {
            let order = kind - 31;
            if order > block {
                return Err(Error::InvalidData);
            }
            for v in &mut s[..order as usize] {
                *v = b.read_signed(bps);
            }
            let precision = b.read(4) + 1;
            let shift = b.read_signed(5);
            if precision == 16 || shift < 0 {
                return Err(Error::InvalidData);
            }
            let mut coef = [0i32; MAX_ORDER];
            let coef = &mut coef[..order as usize];
            for c in coef.iter_mut() {
                *c = b.read_signed(precision);
            }
            read_residual(b, s, block, order)?;
            let wide = bps + precision + bit_length(order) > 32;
            restore_lpc(s, coef, shift as u32, wide);
        }
        _ => return Err(Error::InvalidData),
    }
    if b.overrun() {
        return Err(Error::InvalidSize);
    }
    if wasted != 0 {
        for v in s.iter_mut() {
            *v = ((*v as u32) << wasted) as i32;
        }
    }
    Ok(())
}

fn decorrelate(a: &mut [i32], b: &mut [i32], assignment: u8) {
    for (a, b) in a.iter_mut().zip(b) {
        let (x, y) = (*a as u32, *b as u32);
        match assignment {
            8 => *b = x.wrapping_sub(y) as i32,
            9 => *a = x.wrapping_add(y) as i32,
            10 => {
                let mid = x << 1 | (y & 1);
                *a = (mid.wrapping_add(y) as i32) >> 1;
                *b = (mid.wrapping_sub(y) as i32) >> 1;
            }
            _ => return,
        }
    }
}

impl FlacDecoder {
    pub fn new(config: &FlacConfig<'_>) -> Result<Self> {
        let si = config.streaminfo;
        if si.len() < FLAC_STREAMINFO_BYTES {
            return Err(Error::InvalidArg);
        }
        let max_block = u32::from(si[2]) << 8 | u32::from(si[3]);
        let rate = u32::from(si[10]) << 12 | u32::from(si[11]) << 4 | u32::from(si[12] >> 4);
        let channels = ((si[12] >> 1) & 7) + 1;
        let bps = (((si[12] & 1) << 4) | si[13] >> 4) + 1;
        if max_block == 0 || rate == 0 || !(4..=MAX_BPS).contains(&bps) {
            return Err(Error::NotSupported);
        }
        Ok(Self {
            format: if bps <= 16 { Format::S16 } else { Format::S32 },
            channels,
            max_frames: max_block as usize,
            rate,
            bps,
            samples: heap::zeroed_slice(max_block as usize * usize::from(channels), config.caps)?,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn read_header(&self, b: &mut Bits<'_>, frame: &[u8]) -> Result<FrameHeader> {
        if b.read(14) != 0x3FFE || b.read(1) != 0 {
            return Err(Error::InvalidData);
        }
        b.read(1);
        let block_code = b.read(4);
        let rate_code = b.read(4);
        let assignment = b.read(4);
        let size_code = b.read(3);
        if b.read(1) != 0 {
            return Err(Error::InvalidData);
        }

        let lead = b.read(8);
        let mut extra = 0;
        while extra < 8 && lead & (0x80 >> extra) != 0 {
            extra += 1;
        }
        if extra == 1 || extra == 8 {
            return Err(Error::InvalidData);
        }
        for _ in 1..extra {
            if b.read(8) >> 6 != 2 {
                return Err(Error::InvalidData);
            }
        }

        let block = match block_code {
            0 => return Err(Error::InvalidData),
            1 => 192,
            2..=5 => 576 << (block_code - 2),
            6 => b.read(8) + 1,
            7 => b.read(16) + 1,
            _ => 256 << (block_code - 8),
        };

        let mut rate = self.rate;
        match rate_code {
            15 => return Err(Error::InvalidData),
            1..=11 => rate = RATES[rate_code as usize],
            12 => rate = b.read(8) * 1000,
            13 => rate = b.read(16),
            14 => rate = b.read(16) * 10,
            _ => {}
        }

        let len = b.consumed();
        let crc = b.read(8);
        if b.overrun() {
            return Err(Error::InvalidSize);
        }
        if crc != u32::from(crc8(&frame[..len])) {
            return Err(Error::InvalidCrc);
        }

        let channels = if assignment < 8 { assignment as u8 + 1 } else { 2 };
        let bps = if size_code != 0 { SIZES[size_code as usize] } else { self.bps };
        if assignment > 10 || channels != self.channels || rate != self.rate || bps != self.bps {
            return Err(Error::NotSupported);
        }
        if block as usize > self.max_frames {
            return Err(Error::InvalidSize);
        }
        Ok(FrameHeader { block, assignment: assignment as u8 })
    }

    fn decode_samples(&mut self, frame: &[u8]) -> Result<FrameHeader> {
        let mut b = Bits::new(frame);
        let h = self.read_header(&mut b, frame)?;
        let stride = self.max_frames;
        for (ch, s) in self.samples.chunks_exact_mut(stride).enumerate() {
            let side = (h.assignment == 8 && ch == 1) || (h.assignment == 9 && ch == 0) || (h.assignment == 10 && ch == 1);
            read_subframe(&mut b, s, h.block, u32::from(self.bps) + u32::from(side))?;
        }
        let padding = b.bits % 8;
        b.read(padding);
        let body = b.consumed();
        let crc = b.read(16);
        if b.overrun() {
            return Err(Error::InvalidSize);
        }
        if crc != u32::from(crc16(&frame[..body])) {
            return Err(Error::InvalidCrc);
        }
        if self.channels == 2 {
            let (a, c) = self.samples.split_at_mut(stride);
            decorrelate(&mut a[..h.block as usize], &mut c[..h.block as usize], h.assignment);
        }
        Ok(h)
    }

    fn write<T>(&self, block: usize, pcm: &mut [T], store: impl Fn(u32) -> T) -> Result<usize> {
        let channels = usize::from(self.channels);
        if pcm.len() < block * channels {
            return Err(Error::InvalidSize);
        }
        for (ch, s) in self.samples.chunks_exact(self.max_frames).enumerate() {
            for (i, &v) in s[..block].iter().enumerate() {
                pcm[i * channels + ch] = store(v as u32);
            }
        }
        Ok(block)
    }

    /// A frame whose rate, channels or sample size differ from STREAMINFO
    /// fails, and one that fails its CRC returns `InvalidCrc` with `pcm` left
    /// untouched. For a stream of 16 bits or fewer.
    pub fn decode_s16(&mut self, frame: &[u8], pcm: &mut [i16]) -> Result<usize> {
        if self.format != Format::S16 {
            return Err(Error::InvalidArg);
        }
        let h = self.decode_samples(frame)?;
        let shift = 16 - u32::from(self.bps);
        self.write(h.block as usize, pcm, |v| (v << shift) as i16)
    }

    /// As [`decode_s16`](Self::decode_s16), for a stream of more than 16 bits.
    pub fn decode_s32(&mut self, frame: &[u8], pcm: &mut [i32]) -> Result<usize> {
        if self.format != Format::S32 {
            return Err(Error::InvalidArg);
        }
        let h = self.decode_samples(frame)?;
        let shift = 24 - u32::from(self.bps);
        self.write(h.block as usize, pcm, |v| (v << shift) as i32)
    }
}

impl Decoder for FlacDecoder {
    fn decode(&mut self, frame: &[u8], pcm: PcmMut<'_>) -> Result<usize> {
        match pcm {
            PcmMut::S16(pcm) => self.decode_s16(frame, pcm),
            PcmMut::S32(pcm) => self.decode_s32(frame, pcm),
        }
    }

    fn reset(&mut self) {}

    fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn format(&self) -> Format {
        self.format
    }

    fn channels(&self) -> u8 {
        self.channels
    }
}

