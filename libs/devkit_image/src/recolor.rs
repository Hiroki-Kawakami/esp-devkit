/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::math::{lroundf, powf};

use crate::{Error, PixelFormat, Result};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Luma {
    Rec709,
    Rec601,
    Average,
    Custom([f32; 3]),
}

/// What input pixels are, on their way to linear light.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GammaIn {
    Srgb,
    Power(f32),
    Linear,
}

/// What linear luminance becomes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GammaOut {
    Srgb,
    Power(f32),
    /// Measured reflectance per level of a 16-level panel.
    EpdLut([u8; 16]),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RecolorOptions {
    pub luma: Luma,
    pub in_gamma: GammaIn,
    pub out_gamma: GammaOut,
    /// Average in linear light: `to_intensity` defers the output gamma to
    /// `finalize`.
    pub linear_downsample: bool,
    pub invert: bool,
}

impl Default for RecolorOptions {
    fn default() -> Self {
        Self {
            luma: Luma::Rec709,
            in_gamma: GammaIn::Srgb,
            out_gamma: GammaOut::Srgb,
            linear_downsample: true,
            invert: false,
        }
    }
}

/// Color to perceptual gray in two phases, so that the work straddles a box
/// downscale and stays photometrically correct: `to_intensity` gives a value
/// safe to area-average, a `Gray8` resizer averages it, `finalize` gives the
/// final gray.
#[derive(Clone)]
pub struct Recolor {
    in_lut: [u16; 256],
    out_lut: [u8; 256],
    weights: [u32; 3],
    linear_downsample: bool,
    invert: bool,
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { powf((c + 0.055) / 1.055, 2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { 12.92 * c } else { 1.055 * powf(c, 1.0 / 2.4) - 0.055 }
}

fn clamp01(v: f32) -> f32 {
    if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

impl Recolor {
    pub fn new(options: &RecolorOptions) -> Self {
        let mut in_lut = [0; 256];
        for (i, entry) in in_lut.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            let linear = match options.in_gamma {
                GammaIn::Power(power) => powf(c, power),
                GammaIn::Linear => c,
                GammaIn::Srgb => srgb_to_linear(c),
            };
            *entry = lroundf(clamp01(linear) * 65535.0) as u16;
        }

        let mut out_lut = [0; 256];
        for (i, entry) in out_lut.iter_mut().enumerate() {
            let linear = i as f32 / 255.0;
            let gray = match options.out_gamma {
                GammaOut::Power(power) => powf(linear, 1.0 / power),
                GammaOut::EpdLut(lut) => {
                    let fk = linear * 15.0;
                    let k = fk as usize;
                    if k >= 15 {
                        f32::from(lut[15]) / 255.0
                    } else {
                        let t = fk - k as f32;
                        ((1.0 - t) * f32::from(lut[k]) + t * f32::from(lut[k + 1])) / 255.0
                    }
                }
                GammaOut::Srgb => linear_to_srgb(linear),
            };
            *entry = lroundf(clamp01(gray) * 255.0) as u8;
        }

        const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];
        let mut w = match options.luma {
            Luma::Rec601 => [0.299, 0.587, 0.114],
            Luma::Average => [1.0 / 3.0; 3],
            Luma::Custom(weights) => weights,
            Luma::Rec709 => REC709,
        };
        let mut sum = w[0] + w[1] + w[2];
        if sum <= 0.0 {
            w = REC709;
            sum = 1.0;
        }
        let wr = lroundf(w[0] / sum * 32768.0) as u32;
        let wg = lroundf(w[1] / sum * 32768.0) as u32;
        let weights = [wr, wg, 32768u32.wrapping_sub(wr).wrapping_sub(wg)];

        Self { in_lut, out_lut, weights, linear_downsample: options.linear_downsample, invert: options.invert }
    }

    /// `format` is `Gray8` or `Rgb888`; one intensity per pixel of `src`.
    pub fn to_intensity(&self, src: &[u8], format: PixelFormat, intensity: &mut [u8]) -> Result<()> {
        let [wr, wg, wb] = self.weights;
        let out = |lum: u32| if self.linear_downsample { (lum >> 8) as u8 } else { self.out_lut[(lum >> 8) as usize] };
        match format {
            PixelFormat::Gray8 => {
                for (dst, &v) in intensity.iter_mut().zip(src) {
                    *dst = out(u32::from(self.in_lut[usize::from(v)]));
                }
            }
            PixelFormat::Rgb888 => {
                for (dst, p) in intensity.iter_mut().zip(src.chunks_exact(3)) {
                    let [r, g, b] = [p[0], p[1], p[2]].map(|v| u32::from(self.in_lut[usize::from(v)]));
                    *dst = out(wr.wrapping_mul(r).wrapping_add(wg.wrapping_mul(g)).wrapping_add(wb.wrapping_mul(b)) >> 15);
                }
            }
            _ => return Err(Error::InvalidArgument),
        }
        Ok(())
    }

    fn final_gray(&self, intensity: u8) -> u8 {
        let gray = if self.linear_downsample { self.out_lut[usize::from(intensity)] } else { intensity };
        if self.invert { 255 - gray } else { gray }
    }

    pub fn finalize(&self, intensity: &[u8], dst: &mut [u8]) {
        for (out, &v) in dst.iter_mut().zip(intensity) {
            *out = self.final_gray(v);
        }
    }

    pub fn finalize_in_place(&self, row: &mut [u8]) {
        for v in row {
            *v = self.final_gray(*v);
        }
    }
}
