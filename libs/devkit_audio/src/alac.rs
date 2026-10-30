/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::heap::{self, Caps};

use crate::{Decoder, Error, Format, PcmMut, Result};

pub const ALAC_COOKIE_BYTES: usize = 24;

const MAX_BITS: u8 = 24;
const MAX_CHANNELS: u8 = 8;
const MAX_FRAME: u32 = 65536;
const MAX_ORDER: usize = 31;

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

const QBSHIFT: u32 = 9;
const QB: u32 = 1 << QBSHIFT;
const MMULSHIFT: u32 = 2;
const MDENSHIFT: u32 = QBSHIFT - MMULSHIFT - 1;
const MOFF: u32 = 1 << (MDENSHIFT - 2);
const BITOFF: u32 = 24;
const MAX_PREFIX: u32 = 9;
const RUN_ESCAPE_BITS: u32 = 16;
const MEAN_CLAMP: u32 = 0xffff;

#[derive(Clone, Copy, Debug)]
pub struct AlacConfig<'a> {
    /// The ALACSpecificConfig, without any atom header.
    pub cookie: &'a [u8],
    pub caps: Caps,
}

/// ALAC up to 24 bits and 8 channels. 16 bits or fewer decode to S16, more to
/// S32, both scaled to full scale. Channels come in the order of the frame's
/// elements.
pub struct AlacDecoder {
    format: Format,
    channels: u8,
    max_frames: usize,
    rate: u32,
    bits: u8,
    pb: u8,
    mb: u8,
    kb: u8,
    // [predictor][mix 0][mix 1 when stereo], max_frames each.
    work: Box<[i32], Caps>,
}

#[derive(Clone, Copy)]
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    #[inline(always)]
    fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let shift = 8 - (self.pos & 7) as u32;
        if let Some(p) = self.data.get(byte..byte + 5) {
            let hi = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
            return if shift == 8 { hi } else { hi << (8 - shift) | u32::from(p[4]) >> shift };
        }
        let mut v = 0u64;
        for i in 0..5 {
            v = v << 8 | u64::from(self.data.get(byte + i).copied().unwrap_or(0));
        }
        (v >> shift) as u32
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek32() >> (32 - n);
        self.pos += n as usize;
        v
    }

    fn read_signed(&mut self, n: u32) -> i32 {
        let v = self.read(n) << (32 - n);
        (v as i32) >> (32 - n)
    }

    fn overrun(&self) -> bool {
        self.pos > self.data.len() * 8
    }

    #[inline(always)]
    fn read_golomb(&mut self, m: u32, k: u32, escape_bits: u32) -> u32 {
        let stream = self.peek32();
        let prefix = (!stream).leading_zeros();
        if prefix >= MAX_PREFIX {
            self.pos += MAX_PREFIX as usize;
            return self.read(escape_bits);
        }
        self.pos += prefix as usize + 1;
        let v = self.peek32() >> (32 - k);
        if v < 2 {
            self.pos += k as usize - 1;
            return prefix.wrapping_mul(m);
        }
        self.pos += k as usize;
        (prefix.wrapping_mul(m).wrapping_add(v)).wrapping_sub(1)
    }
}

struct Element {
    frames: usize,
    mix_bits: u32,
    mix_res: i32,
    shift: u32,
}

#[derive(Clone, Copy)]
struct Channel {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    order: usize,
    coefs: [i16; MAX_ORDER],
}

fn read_residual(b: &mut Bits<'_>, out: &mut [i32], mb0: u32, kb: u32, pb: u32, escape_bits: u32) -> Result<()> {
    let n = out.len();
    let wb = (1u32 << kb).wrapping_sub(1);
    let mut mb = mb0;
    let mut zmode = 0u32;
    let mut c = 0;
    while c < n {
        if b.pos >= b.data.len() * 8 {
            return Err(Error::InvalidSize);
        }
        let k = (31 - ((mb >> QBSHIFT) + 3).leading_zeros()).min(kb);
        let v = b.read_golomb((1u32 << k) - 1, k, escape_bits);
        let nd = v.wrapping_add(zmode);
        let half = nd.wrapping_add(1) >> 1;
        out[c] = (if nd & 1 != 0 { 0u32.wrapping_sub(half) } else { half }) as i32;
        c += 1;
        mb = pb.wrapping_mul(nd).wrapping_add(mb).wrapping_sub(pb.wrapping_mul(mb) >> QBSHIFT);
        if v > MEAN_CLAMP {
            mb = MEAN_CLAMP;
        }
        zmode = 0;
        if (mb << MMULSHIFT) < QB && c < n {
            zmode = 1;
            let lead = mb.leading_zeros();
            let k = (lead + ((mb + MOFF) >> MDENSHIFT)).wrapping_sub(BITOFF);
            if !(1..=31).contains(&k) {
                return Err(Error::InvalidData);
            }
            let run = b.read_golomb(((1u32 << k) - 1) & wb, k, RUN_ESCAPE_BITS);
            if run as usize > n - c {
                return Err(Error::InvalidData);
            }
            out[c..c + run as usize].fill(0);
            c += run as usize;
            if run >= 65535 {
                zmode = 0;
            }
            mb = 0;
        }
    }
    if b.overrun() { Err(Error::InvalidSize) } else { Ok(()) }
}

#[inline(always)]
fn clip(v: u32, clip: u32) -> i32 {
    ((v << clip) as i32) >> clip
}

fn integrate(buf: &mut [i32], bits: u32) {
    let shift = 32 - bits;
    let mut prev = buf[0];
    for v in &mut buf[1..] {
        prev = clip((*v as u32).wrapping_add(prev as u32), shift);
        *v = prev;
    }
}

fn predict(input: &[i32], out: &mut [i32], coefs: &mut [i16; MAX_ORDER], order: usize, bits: u32, den_shift: u32) {
    let n = out.len();
    let shift = 32 - bits;
    out[0] = input[0];
    if order == 0 {
        out[1..].copy_from_slice(&input[1..n]);
        return;
    }
    if order == MAX_ORDER {
        out[1..].copy_from_slice(&input[1..n]);
        integrate(out, bits);
        return;
    }
    let warm = (order + 1).min(n);
    for j in 1..warm {
        out[j] = clip((input[j] as u32).wrapping_add(out[j - 1] as u32), shift);
    }

    let half = if den_shift != 0 { 1u32 << (den_shift - 1) } else { 0 };
    let p = out.as_mut_ptr();
    let c = coefs.as_mut_ptr();
    for j in order + 1..n {
        unsafe {
            let x = p.add(j - 1);
            let top = *p.add(j - order - 1);
            let mut sum = 0u32;
            for k in 0..order {
                sum = sum.wrapping_add((i32::from(*c.add(k)) as u32).wrapping_mul((*x.sub(k)).wrapping_sub(top) as u32));
            }

            let del = *input.get_unchecked(j);
            let pred = ((sum.wrapping_add(half)) as i32) >> den_shift;
            *p.add(j) = clip((del as u32).wrapping_add(top as u32).wrapping_add(pred as u32), shift);

            let mut rest = del;
            if del > 0 {
                for k in (0..order).rev() {
                    let dd = top.wrapping_sub(*x.sub(k));
                    let sgn = dd.signum();
                    *c.add(k) = (*c.add(k)).wrapping_sub(sgn as i16);
                    rest = rest.wrapping_sub(((order - k) as i32).wrapping_mul(sgn.wrapping_mul(dd) >> den_shift));
                    if rest <= 0 {
                        break;
                    }
                }
            } else if del < 0 {
                for k in (0..order).rev() {
                    let dd = top.wrapping_sub(*x.sub(k));
                    let sgn = dd.signum();
                    *c.add(k) = (*c.add(k)).wrapping_add(sgn as i16);
                    rest = rest.wrapping_sub(((order - k) as i32).wrapping_mul((-sgn).wrapping_mul(dd) >> den_shift));
                    if rest >= 0 {
                        break;
                    }
                }
            }
        }
    }
}

impl AlacDecoder {
    pub fn new(config: &AlacConfig<'_>) -> Result<Self> {
        let c = config.cookie;
        if c.len() < ALAC_COOKIE_BYTES {
            return Err(Error::InvalidArg);
        }
        let frame_length = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        let (version, bits, kb, channels) = (c[4], c[5], c[8], c[9]);
        let rate = u32::from_be_bytes([c[20], c[21], c[22], c[23]]);
        if version != 0
            || frame_length == 0
            || frame_length > MAX_FRAME
            || bits == 0
            || bits > MAX_BITS
            || kb > 31
            || channels == 0
            || channels > MAX_CHANNELS
            || rate == 0
        {
            return Err(Error::NotSupported);
        }
        let buffers = if channels > 1 { 3 } else { 2 };
        Ok(Self {
            format: if bits <= 16 { Format::S16 } else { Format::S32 },
            channels,
            max_frames: frame_length as usize,
            rate,
            bits,
            pb: c[6],
            mb: c[7],
            kb,
            work: heap::zeroed_slice(buffers * frame_length as usize, config.caps)?,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn read_element(&mut self, b: &mut Bits<'_>, count: usize) -> Result<Element> {
        b.read(4);
        if b.read(12) != 0 {
            return Err(Error::InvalidData);
        }
        let head = b.read(4);
        let shifted = (head >> 1) & 3;
        if shifted == 3 {
            return Err(Error::InvalidData);
        }
        let mut frames = self.max_frames as u32;
        if head & 8 != 0 {
            frames = b.read(32);
        }
        if frames == 0 {
            return Err(Error::InvalidData);
        }
        if frames as usize > self.max_frames {
            return Err(Error::InvalidSize);
        }
        let frames = frames as usize;
        let mut e = Element { frames, mix_bits: 0, mix_res: 0, shift: shifted * 8 };
        let bits = u32::from(self.bits);
        if e.shift >= bits {
            return Err(Error::InvalidData);
        }
        let stride = self.max_frames;
        let (predictor, mix) = self.work.split_at_mut(stride);

        if head & 1 != 0 {
            let width = if count == 1 { bits - e.shift } else { bits };
            for i in 0..frames {
                for c in 0..count {
                    mix[c * stride + i] = b.read_signed(width);
                }
            }
            e.shift = 0;
            return if b.overrun() { Err(Error::InvalidSize) } else { Ok(e) };
        }

        if self.kb == 0 {
            return Err(Error::InvalidData);
        }
        e.mix_bits = b.read(8);
        e.mix_res = i32::from(b.read(8) as i8);
        if e.mix_bits > 31 {
            return Err(Error::InvalidData);
        }
        let mut ch = [Channel { mode: 0, den_shift: 0, pb_factor: 0, order: 0, coefs: [0; MAX_ORDER] }; 2];
        for c in &mut ch[..count] {
            let h = b.read(8);
            c.mode = h >> 4;
            c.den_shift = h & 15;
            let h = b.read(8);
            c.pb_factor = h >> 5;
            c.order = (h & 31) as usize;
            let order = c.order;
            for v in &mut c.coefs[..order] {
                *v = b.read(16) as i16;
            }
        }
        let mut low = *b;
        b.pos += e.shift as usize * count * frames;

        let width = bits - e.shift + count as u32 - 1;
        for (c, channel) in ch[..count].iter_mut().enumerate() {
            let pb = u32::from(self.pb) * channel.pb_factor / 4;
            read_residual(b, &mut predictor[..frames], u32::from(self.mb), u32::from(self.kb), pb, width)?;
            if channel.mode != 0 {
                integrate(&mut predictor[..frames], width);
            }
            let out = &mut mix[c * stride..c * stride + frames];
            predict(&predictor[..frames], out, &mut channel.coefs, channel.order, width, channel.den_shift);
        }
        if e.shift != 0 {
            for i in 0..frames * count {
                let v = low.read(e.shift) as u16;
                let word = &mut predictor[i / 2];
                let half = (i & 1) as u32 * 16;
                *word = ((*word as u32 & !(0xffff << half)) | u32::from(v) << half) as i32;
            }
        }
        Ok(e)
    }

    fn write_element<T>(&self, e: &Element, count: usize, pcm: &mut [T], first: usize, store: impl Fn(i32) -> T) {
        let channels = usize::from(self.channels);
        let stride = self.max_frames;
        let (predictor, mix) = self.work.split_at(stride);
        let (u, v) = mix.split_at(stride);
        for i in 0..e.frames {
            let mut s = [u[i], 0];
            if count == 2 {
                s[1] = v[i];
                if e.mix_res != 0 {
                    s[0] = u[i].wrapping_add(v[i]).wrapping_sub(e.mix_res.wrapping_mul(v[i]) >> e.mix_bits);
                    s[1] = s[0].wrapping_sub(v[i]);
                }
            }
            for c in 0..count {
                let mut x = s[c];
                if e.shift != 0 {
                    let at = i * count + c;
                    let low = (predictor[at / 2] as u32 >> ((at & 1) as u32 * 16)) & 0xffff;
                    x = ((x as u32) << e.shift | low) as i32;
                }
                pcm[i * channels + first + c] = store(x);
            }
        }
    }

    fn decode_into<T>(&mut self, frame: &[u8], pcm: &mut [T], store: impl Fn(i32) -> T + Copy) -> Result<usize> {
        if pcm.len() < self.max_frames * usize::from(self.channels) {
            return Err(Error::InvalidSize);
        }
        let mut b = Bits { data: frame, pos: 0 };
        let mut ch = 0usize;
        let mut total = 0;
        while ch < usize::from(self.channels) {
            if b.pos >= frame.len() * 8 {
                return Err(Error::InvalidSize);
            }
            let tag = b.read(3);
            match tag {
                ID_SCE | ID_LFE | ID_CPE => {
                    let count = if tag == ID_CPE { 2 } else { 1 };
                    if ch + count > usize::from(self.channels) {
                        return Err(Error::NotSupported);
                    }
                    let e = self.read_element(&mut b, count)?;
                    if total != 0 && e.frames != total {
                        return Err(Error::InvalidData);
                    }
                    total = e.frames;
                    self.write_element(&e, count, pcm, ch, store);
                    ch += count;
                }
                ID_DSE => {
                    b.read(4);
                    let align = b.read(1) != 0;
                    let mut count = b.read(8);
                    if count == 255 {
                        count += b.read(8);
                    }
                    if align {
                        b.pos = (b.pos + 7) & !7;
                    }
                    b.pos += count as usize * 8;
                }
                ID_FIL => {
                    let mut count = b.read(4);
                    if count == 15 {
                        count = (count + b.read(8)).wrapping_sub(1);
                    }
                    b.pos += count as usize * 8;
                }
                ID_END => return Err(Error::NotSupported),
                _ => return Err(Error::InvalidData),
            }
        }
        Ok(total)
    }

    /// For a stream of 16 bits or fewer.
    pub fn decode_s16(&mut self, frame: &[u8], pcm: &mut [i16]) -> Result<usize> {
        if self.format != Format::S16 {
            return Err(Error::InvalidArg);
        }
        let shift = 16 - u32::from(self.bits);
        self.decode_into(frame, pcm, |v| ((v as u32) << shift) as i16)
    }

    /// For a stream of more than 16 bits.
    pub fn decode_s32(&mut self, frame: &[u8], pcm: &mut [i32]) -> Result<usize> {
        if self.format != Format::S32 {
            return Err(Error::InvalidArg);
        }
        let shift = 24 - u32::from(self.bits);
        self.decode_into(frame, pcm, |v| ((v as u32) << shift) as i32)
    }
}

impl Decoder for AlacDecoder {
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
