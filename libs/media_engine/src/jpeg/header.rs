/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Baseline JPEG headers, read up to the scan the way IDF's decoder reads them.
 */

use idf::{Error, Result};

const MAX_TABLES: usize = 4;
/// What the 2D-DMA descriptor fields hold.
const MAX_SIZE: u32 = (1 << 14) - 1;

const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47,
    55, 62, 63,
];

const LUMA_DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const CHROMA_DC_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const LUMA_AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const CHROMA_AC_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const LUMA_AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14,
    0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09,
    0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a,
    0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65,
    0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9,
    0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
    0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea,
    0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];
const CHROMA_AC_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32,
    0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16,
    0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39,
    0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
    0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86,
    0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8,
    0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9,
    0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sampling {
    Gray,
    Yuv444,
    Yuv422,
    Yuv420,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub table: u8,
}

/// What a baseline JPEG says before its scan.
#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub width: u16,
    pub height: u16,
    /// Rounded up to whole MCUs, as the decoder writes them.
    pub padded_width: u16,
    pub padded_height: u16,
    pub mcu: (u8, u8),
    pub sampling: Sampling,
    /// Where the scan, from its SOS marker on, starts.
    pub(crate) scan: usize,
}

/// What the codec is programmed with. Kept apart from `Header`, which is
/// copied around, so it can be read straight into the codec's own copy.
pub(crate) struct Tables {
    pub components: [Component; MAX_TABLES],
    pub component_count: u8,
    pub quant: [[u32; 64]; MAX_TABLES],
    pub quant_count: u8,
    /// `[dc, ac][luma, chroma]`.
    pub huff_bits: [[[u8; 16]; 2]; 2],
    pub huff_values: [[[u8; 162]; 2]; 2],
    pub restart_interval: u16,
}

#[cfg_attr(not(target_os = "none"), allow(dead_code))]
impl Tables {
    pub const fn new() -> Self {
        Self {
            components: [Component { id: 0, h: 0, v: 0, table: 0 }; MAX_TABLES],
            component_count: 0,
            quant: [[0; 64]; MAX_TABLES],
            quant_count: 0,
            huff_bits: [[[0; 16]; 2]; 2],
            huff_values: [[[0; 162]; 2]; 2],
            restart_interval: 0,
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn left(&self) -> usize {
        self.data.len() - self.at
    }

    fn u8(&mut self) -> Result<u8> {
        let b = *self.data.get(self.at).ok_or(Error::INVALID_ARG)?;
        self.at += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from(self.u8()?) << 8 | u16::from(self.u8()?))
    }

    /// A segment's length field, less itself; an error past the data.
    fn length(&mut self) -> Result<usize> {
        let n = usize::from(self.u16()?);
        if n < 2 || n - 2 > self.left() {
            return Err(Error::INVALID_ARG);
        }
        Ok(n - 2)
    }

    fn skip(&mut self, n: usize) {
        self.at += n;
    }
}

impl Header {
    pub fn parse(data: &[u8]) -> Result<Self> {
        parse(data, None)
    }
}

/// The header, and the tables into `tables` when given.
pub(crate) fn parse(data: &[u8], mut tables: Option<&mut Tables>) -> Result<Header> {
    let mut h = Header { width: 0, height: 0, padded_width: 0, padded_height: 0, mcu: (8, 8), sampling: Sampling::Gray, scan: 0 };
    if let Some(t) = tables.as_deref_mut() {
        t.quant_count = 0;
        t.restart_interval = 0;
    }
    let mut r = Reader { data, at: 0 };
    let (mut quant_seen, mut huffman, mut frame) = (0u8, false, false);
    loop {
        if r.left() < 2 {
            return Err(Error::INVALID_ARG);
        }
        match r.u16()? {
            0xFFD8 => {}
            0xFFE0..=0xFFEF | 0xFFFE => {
                let n = r.length()?;
                r.skip(n);
            }
            0xFFDB => quant_tables(&mut r, &mut quant_seen, tables.as_deref_mut())?,
            0xFFC0 => {
                frame_header(&mut r, &mut h, tables.as_deref_mut())?;
                frame = true;
            }
            0xFFC1..=0xFFC3 | 0xFFC5..=0xFFC7 | 0xFFC9..=0xFFCB | 0xFFCD..=0xFFCF => return Err(Error::NOT_SUPPORTED),
            0xFFDD => {
                if r.u16()? != 4 {
                    return Err(Error::INVALID_SIZE);
                }
                let interval = r.u16()?;
                if let Some(t) = tables.as_deref_mut() {
                    t.restart_interval = interval;
                }
            }
            0xFFC4 => {
                huffman_tables(&mut r, tables.as_deref_mut())?;
                huffman = true;
            }
            0xFFDA => {
                h.scan = r.at - 2;
                break;
            }
            // A stray 0xFF before a marker: read the marker from the second one.
            0xFFFF => r.at -= 1,
            _ => {}
        }
    }
    if !frame {
        return Err(Error::INVALID_ARG);
    }
    if !huffman && let Some(t) = tables {
        t.huff_bits = [[LUMA_DC_BITS, CHROMA_DC_BITS], [LUMA_AC_BITS, CHROMA_AC_BITS]];
        for values in &mut t.huff_values[0] {
            values[..12].copy_from_slice(&DC_VALUES);
        }
        t.huff_values[1] = [LUMA_AC_VALUES, CHROMA_AC_VALUES];
    }
    Ok(h)
}

fn quant_tables(r: &mut Reader<'_>, seen: &mut u8, mut tables: Option<&mut Tables>) -> Result<()> {
    let mut left = r.length()?;
    while left > 0 {
        let pt = r.u8()?;
        left -= 1;
        let (precision, id) = (pt >> 4, usize::from(pt & 0x0F));
        if id >= MAX_TABLES || precision > 1 {
            return Err(Error::INVALID_ARG);
        }
        let bytes = 64 * (usize::from(precision) + 1);
        if left < bytes {
            return Err(Error::INVALID_ARG);
        }
        for &z in &ZIGZAG {
            let mut q = u32::from(r.u8()?);
            if precision == 1 {
                q = q << 8 | u32::from(r.u8()?);
            }
            if let Some(t) = tables.as_deref_mut() {
                t.quant[id][usize::from(z)] = q;
            }
        }
        left -= bytes;
        if *seen & 1 << id == 0 {
            *seen |= 1 << id;
            if let Some(t) = tables.as_deref_mut() {
                t.quant_count += 1;
            }
        }
    }
    Ok(())
}

fn frame_header(r: &mut Reader<'_>, h: &mut Header, tables: Option<&mut Tables>) -> Result<()> {
    let n = r.length()?;
    if n < 6 {
        return Err(Error::INVALID_ARG);
    }
    if r.u8()? != 8 {
        return Err(Error::NOT_SUPPORTED);
    }
    let (height, width) = (r.u16()?, r.u16()?);
    if width == 0 || height == 0 || u32::from(width) > MAX_SIZE || u32::from(height) > MAX_SIZE {
        return Err(Error::INVALID_ARG);
    }
    if u32::from(width) * u32::from(height) % 8 != 0 {
        return Err(Error::NOT_SUPPORTED);
    }
    let count = r.u8()?;
    if count == 0 || usize::from(count) >= MAX_TABLES {
        return Err(Error::NOT_SUPPORTED);
    }
    if n < 6 + 3 * usize::from(count) {
        return Err(Error::INVALID_ARG);
    }
    let mut components = [Component::default(); MAX_TABLES];
    for c in &mut components[..usize::from(count)] {
        c.id = r.u8()?;
        let hv = r.u8()?;
        (c.h, c.v) = (hv >> 4, hv & 0x0F);
        c.table = r.u8()?;
        if usize::from(c.table) >= MAX_TABLES {
            return Err(Error::INVALID_ARG);
        }
    }
    r.skip(n - 6 - 3 * usize::from(count));
    let first = components[0];
    if first.h == 0 || first.v == 0 {
        return Err(Error::INVALID_ARG);
    }
    h.mcu = (first.h * 8, first.v * 8);
    h.sampling = match (count, first.h, first.v) {
        (1, _, _) => Sampling::Gray,
        (3, 1, 1) => Sampling::Yuv444,
        (3, 2, 1) => Sampling::Yuv422,
        (3, 2, 2) => Sampling::Yuv420,
        _ => return Err(Error::NOT_SUPPORTED),
    };
    h.width = width;
    h.height = height;
    h.padded_width = width.next_multiple_of(u16::from(h.mcu.0));
    h.padded_height = height.next_multiple_of(u16::from(h.mcu.1));
    if u32::from(h.padded_width) > MAX_SIZE || u32::from(h.padded_height) > MAX_SIZE {
        return Err(Error::INVALID_ARG);
    }
    if let Some(t) = tables {
        t.components = components;
        t.component_count = count;
    }
    Ok(())
}

fn huffman_tables(r: &mut Reader<'_>, mut tables: Option<&mut Tables>) -> Result<()> {
    let mut left = r.length()?;
    while left > 0 {
        if left < 17 {
            return Err(Error::INVALID_ARG);
        }
        let info = r.u8()?;
        let (class, id) = (usize::from(info >> 4), usize::from(info & 0x0F));
        if class > 1 || id > 1 {
            return Err(Error::INVALID_ARG);
        }
        let mut bits = [0u8; 16];
        for b in &mut bits {
            *b = r.u8()?;
        }
        let count = bits.iter().map(|&b| usize::from(b)).sum::<usize>();
        if count > 162 || left < 17 + count {
            return Err(Error::INVALID_ARG);
        }
        match tables.as_deref_mut() {
            Some(t) => {
                t.huff_bits[class][id] = bits;
                for v in &mut t.huff_values[class][id][..count] {
                    *v = r.u8()?;
                }
            }
            None => r.skip(count),
        }
        left -= 17 + count;
    }
    Ok(())
}
