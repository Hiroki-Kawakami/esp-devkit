/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::heap::Caps;

use crate::mem::{Buffer, buffer};
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DitherAlgorithm {
    /// Nearest level.
    #[default]
    None,
    /// Ordered, stateless, stable between frames.
    Bayer2,
    Bayer4,
    Bayer8,
    FloydSteinberg,
    Atkinson,
    Sierra,
    JarvisJudiceNinke,
    Stucki,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DitherOutput {
    /// Level indices `0..levels`, for a packer downstream.
    #[default]
    Index,
    /// The gray each level stands for, `level * 255 / (levels - 1)`.
    Gray,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct DitherOptions {
    pub algorithm: DitherAlgorithm,
    /// 2..=255; 0 means 16.
    pub levels: u8,
    /// Boustrophedon scan, error diffusion only.
    pub serpentine: bool,
    pub output: DitherOutput,
    pub caps: Option<Caps>,
}

#[derive(Clone, Copy)]
struct Cell {
    dx: i32,
    dy: i32,
    weight: i32,
}

const fn c(dx: i32, dy: i32, weight: i32) -> Cell {
    Cell { dx, dy, weight }
}

const FS: [Cell; 4] = [c(1, 0, 7), c(-1, 1, 3), c(0, 1, 5), c(1, 1, 1)];
const ATKINSON: [Cell; 6] = [c(1, 0, 1), c(2, 0, 1), c(-1, 1, 1), c(0, 1, 1), c(1, 1, 1), c(0, 2, 1)];
const SIERRA: [Cell; 10] = [
    c(1, 0, 5), c(2, 0, 3),
    c(-2, 1, 2), c(-1, 1, 4), c(0, 1, 5), c(1, 1, 4), c(2, 1, 2),
    c(-1, 2, 2), c(0, 2, 3), c(1, 2, 2),
];
const JJN: [Cell; 12] = [
    c(1, 0, 7), c(2, 0, 5),
    c(-2, 1, 3), c(-1, 1, 5), c(0, 1, 7), c(1, 1, 5), c(2, 1, 3),
    c(-2, 2, 1), c(-1, 2, 3), c(0, 2, 5), c(1, 2, 3), c(2, 2, 1),
];
const STUCKI: [Cell; 12] = [
    c(1, 0, 8), c(2, 0, 4),
    c(-2, 1, 2), c(-1, 1, 4), c(0, 1, 8), c(1, 1, 4), c(2, 1, 2),
    c(-2, 2, 1), c(-1, 2, 2), c(0, 2, 4), c(1, 2, 2), c(2, 2, 1),
];

const BAYER2: [u8; 4] = [0, 2, 3, 1];
const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
const BAYER8: [u8; 64] = [
    0, 48, 12, 60, 3, 51, 15, 63, 32, 16, 44, 28, 35, 19, 47, 31,
    8, 56, 4, 52, 11, 59, 7, 55, 40, 24, 36, 20, 43, 27, 39, 23,
    2, 50, 14, 62, 1, 49, 13, 61, 34, 18, 46, 30, 33, 17, 45, 29,
    10, 58, 6, 54, 9, 57, 5, 53, 42, 26, 38, 22, 41, 25, 37, 21,
];

impl DitherAlgorithm {
    fn kernel(self) -> (&'static [Cell], i32) {
        match self {
            Self::Atkinson => (&ATKINSON, 8),
            Self::Sierra => (&SIERRA, 32),
            Self::JarvisJudiceNinke => (&JJN, 48),
            Self::Stucki => (&STUCKI, 42),
            _ => (&FS, 16),
        }
    }

    fn bayer(self) -> Option<(&'static [u8], usize)> {
        match self {
            Self::Bayer2 => Some((&BAYER2, 2)),
            Self::Bayer4 => Some((&BAYER4, 4)),
            Self::Bayer8 => Some((&BAYER8, 8)),
            _ => None,
        }
    }
}

/// Quantizes `Gray8` rows to N levels.
pub struct Ditherer {
    width: usize,
    algorithm: DitherAlgorithm,
    levels: i32,
    serpentine: bool,
    output: DitherOutput,
    quantize: [u8; 256],
    reconstruct: [u8; 256],
    bayer_size: usize,
    thresholds: [i32; 64],
    error: Option<Buffer<i32>>,
    ring: usize,
    y: usize,
}

impl Ditherer {
    pub fn new(width: u16, options: &DitherOptions) -> Result<Self> {
        if width == 0 {
            return Err(Error::InvalidArgument);
        }
        let levels = if options.levels == 0 { 16 } else { i32::from(options.levels).max(2) };
        let diff = levels - 1;
        let mut quantize = [0; 256];
        for (v, entry) in quantize.iter_mut().enumerate() {
            *entry = ((v as i32 * diff * 2 + 255) / 510).min(diff) as u8;
        }
        let mut reconstruct = [0; 256];
        for (l, entry) in reconstruct.iter_mut().enumerate().take(levels as usize) {
            *entry = ((l as i32 * 255 + diff / 2) / diff) as u8;
        }

        let mut bayer_size = 0;
        let mut thresholds = [0; 64];
        let mut error = None;
        if let Some((matrix, size)) = options.algorithm.bayer() {
            bayer_size = size;
            let cells = (size * size) as i32;
            for (threshold, &m) in thresholds.iter_mut().zip(matrix) {
                *threshold = (255 * (2 * i32::from(m) + 1) - 255 * cells) / (2 * cells);
            }
        } else if options.algorithm != DitherAlgorithm::None {
            error = Some(buffer(3 * usize::from(width), options.caps)?);
        }

        Ok(Self {
            width: usize::from(width),
            algorithm: options.algorithm,
            levels,
            serpentine: options.serpentine,
            output: options.output,
            quantize,
            reconstruct,
            bayer_size,
            thresholds,
            error,
            ring: 0,
            y: 0,
        })
    }

    pub const fn levels(&self) -> u8 {
        self.levels as u8
    }

    pub const fn output(&self) -> DitherOutput {
        self.output
    }

        pub fn process_row(&mut self, gray: &[u8], dst: &mut [u8]) -> Result<()> {
        if gray.len() < self.width || dst.len() < self.width {
            return Err(Error::InvalidArgument);
        }
        unsafe { self.process(gray.as_ptr(), dst.as_mut_ptr()) };
        Ok(())
    }

    pub fn process_row_in_place(&mut self, row: &mut [u8]) -> Result<()> {
        if row.len() < self.width {
            return Err(Error::InvalidArgument);
        }
        let p = row.as_mut_ptr();
        unsafe { self.process(p, p) };
        Ok(())
    }

    #[inline]
    fn emit(&self, level: i32) -> u8 {
        if self.output == DitherOutput::Gray { self.reconstruct[level as usize] } else { level as u8 }
    }

    // Reads each pixel of `gray` before writing that pixel of `dst`, which is
    // what lets the two be the same row.
    unsafe fn process(&mut self, gray: *const u8, dst: *mut u8) {
        match self.algorithm {
            DitherAlgorithm::None => {
                for x in 0..self.width {
                    let level = self.quantize[usize::from(unsafe { *gray.add(x) })];
                    let value = self.emit(i32::from(level));
                    unsafe { *dst.add(x) = value };
                }
            }
            DitherAlgorithm::Bayer2 | DitherAlgorithm::Bayer4 | DitherAlgorithm::Bayer8 => {
                let diff = self.levels - 1;
                let m = self.bayer_size;
                let yr = self.y % m;
                for x in 0..self.width {
                    let num = i32::from(unsafe { *gray.add(x) }) * diff + self.thresholds[yr * m + x % m];
                    let level = if num <= 0 { 0 } else { ((num * 2 + 255) / 510).min(diff) };
                    let value = self.emit(level);
                    unsafe { *dst.add(x) = value };
                }
            }
            _ => unsafe { self.diffuse(gray, dst) },
        }
        self.y += 1;
    }

    unsafe fn diffuse(&mut self, gray: *const u8, dst: *mut u8) {
        let width = self.width;
        let mut error = self.error.take().unwrap();
        let (kernel, div) = self.algorithm.kernel();
        let shift = if div & (div - 1) == 0 { Some(div.trailing_zeros()) } else { None };
        let rows = [self.ring, (self.ring + 1) % 3, (self.ring + 2) % 3];
        let base = error.as_mut_ptr();
        let row = rows.map(|r| unsafe { base.add(r * width) });

        let dir = if self.serpentine && self.y & 1 != 0 { -1 } else { 1 };
        let (start, end) = if dir > 0 { (0, width as i32) } else { (width as i32 - 1, -1) };
        let mut x = start;
        while x != end {
            let ux = x as usize;
            let vf = (i32::from(unsafe { *gray.add(ux) }) << 8) + unsafe { *row[0].add(ux) };
            let vf = vf.clamp(0, 255 << 8);
            let level = self.quantize[((vf + 128) >> 8) as usize];
            let value = self.emit(i32::from(level));
            unsafe { *dst.add(ux) = value };
            let ef = vf - (i32::from(self.reconstruct[usize::from(level)]) << 8);
            for cell in kernel {
                let tx = x + dir * cell.dx;
                if tx < 0 || tx >= width as i32 {
                    continue;
                }
                let wn = ef * cell.weight;
                let add = match shift {
                    Some(s) => wn >> s,
                    None => wn / div,
                };
                unsafe { *row[cell.dy as usize].add(tx as usize) += add };
            }
            x += dir;
        }

        error[self.ring * width..(self.ring + 1) * width].fill(0);
        self.ring = (self.ring + 1) % 3;
        self.error = Some(error);
    }
}

fn check_strides(w: u16, h: u16, src_stride: usize, dst_stride: usize) -> Result<(usize, usize)> {
    if w == 0 || h == 0 {
        return Err(Error::InvalidArgument);
    }
    let width = usize::from(w);
    let src_stride = if src_stride == 0 { width } else { src_stride };
    let dst_stride = if dst_stride == 0 { width } else { dst_stride };
    if src_stride < width || dst_stride < width {
        return Err(Error::InvalidArgument);
    }
    Ok((src_stride, dst_stride))
}

/// A whole `Gray8` buffer in one call. A stride of 0 is tight packing.
pub fn dither_buffer(src: &[u8], w: u16, h: u16, src_stride: usize, dst: &mut [u8], dst_stride: usize, options: &DitherOptions) -> Result<()> {
    let (src_stride, dst_stride) = check_strides(w, h, src_stride, dst_stride)?;
    let last = usize::from(h) - 1;
    let width = usize::from(w);
    if src.len() < last * src_stride + width || dst.len() < last * dst_stride + width {
        return Err(Error::InvalidArgument);
    }
    let mut d = Ditherer::new(w, options)?;
    for y in 0..usize::from(h) {
        d.process_row(&src[y * src_stride..], &mut dst[y * dst_stride..])?;
    }
    Ok(())
}

pub fn dither_in_place(buf: &mut [u8], w: u16, h: u16, stride: usize, options: &DitherOptions) -> Result<()> {
    let (stride, _) = check_strides(w, h, stride, 0)?;
    if buf.len() < (usize::from(h) - 1) * stride + usize::from(w) {
        return Err(Error::InvalidArgument);
    }
    let mut d = Ditherer::new(w, options)?;
    for y in 0..usize::from(h) {
        d.process_row_in_place(&mut buf[y * stride..])?;
    }
    Ok(())
}
