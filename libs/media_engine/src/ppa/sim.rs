/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The engine on the CPU, as idf_compat's PPA does it: bilinear scaling,
 * straight alpha-over blending, no colour keys. Jobs finish on submit.
 */

use alloc::vec::Vec;
use core::task::{Context, Poll};

use devkit::math::floorf;
use devkit_gfx::{Color, ImageFormat, Rotation};
use idf::{Error, Result};

use super::{Alpha, In, Kind, Plan, SCALE_FRAG, YuvRange, YuvStandard};

pub(super) struct Engines;

impl Engines {
    pub(super) fn new() -> Result<Self> {
        Ok(Self)
    }

    pub(super) fn submit(&'static self, plan: Plan, _node: &mut Node) -> Result<Pending> {
        match plan.kind {
            Kind::Srm => {
                if !readable(plan.input.format, false) || !writable(plan.out.format) {
                    return Err(Error::NOT_SUPPORTED);
                }
                srm(&plan);
            }
            Kind::Blend => {
                if !readable(plan.input.format, false) || !readable(plan.fg.format, true) || !writable(plan.out.format) {
                    return Err(Error::NOT_SUPPORTED);
                }
                blend(&plan);
            }
            Kind::Fill => {
                if !writable(plan.out.format) {
                    return Err(Error::NOT_SUPPORTED);
                }
                fill(&plan);
            }
        }
        Ok(Pending)
    }
}

pub(super) struct Node;

impl Node {
    pub(super) fn new() -> Self {
        Self
    }
}

pub(super) struct Pending;

impl Pending {
    pub(super) fn poll(&mut self, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    pub(super) fn forget_waker(&mut self) {}
}

fn readable(format: ImageFormat, foreground: bool) -> bool {
    match format {
        ImageFormat::Argb8888 | ImageFormat::Bgr888 | ImageFormat::Rgb888 | ImageFormat::Rgb565 => true,
        ImageFormat::Yuv420 => !foreground,
        ImageFormat::A8 => foreground,
        _ => false,
    }
}

fn writable(format: ImageFormat) -> bool {
    matches!(format, ImageFormat::Argb8888 | ImageFormat::Bgr888 | ImageFormat::Rgb565)
}

fn expand5(v: u16) -> u8 {
    ((u32::from(v) * 255 + 15) / 31) as u8
}

fn expand6(v: u16) -> u8 {
    ((u32::from(v) * 255 + 31) / 63) as u8
}

fn narrow5(v: u8) -> u16 {
    ((u32::from(v) * 31 + 127) / 255) as u16
}

fn narrow6(v: u8) -> u16 {
    ((u32::from(v) * 63 + 127) / 255) as u16
}

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// A picture read as straight ARGB, with the input's swaps.
struct Source<'a> {
    pixels: &'a [u8],
    stride: usize,
    format: ImageFormat,
    rgb_swap: bool,
    byte_swap: bool,
    color: Color,
}

impl Source<'_> {
    fn get(&self, x: u32, y: u32) -> Color {
        let bits = self.format.bits();
        let at = y as usize * self.stride + x as usize * bits / 8;
        let mut px = [0u8; 4];
        let n = bits / 8;
        px[..n].copy_from_slice(&self.pixels[at..at + n]);
        if self.byte_swap {
            px[..n].reverse();
        }
        let mut c = match self.format {
            ImageFormat::Argb8888 => Color::rgba(px[2], px[1], px[0], px[3]),
            ImageFormat::Bgr888 => Color::rgb(px[2], px[1], px[0]),
            ImageFormat::Rgb888 => Color::rgb(px[0], px[1], px[2]),
            ImageFormat::Rgb565 => {
                let v = u16::from_le_bytes([px[0], px[1]]);
                Color::rgb(expand5(v >> 11), expand6((v >> 5) & 0x3f), expand5(v & 0x1f))
            }
            ImageFormat::A8 => Color::rgba(self.color.r, self.color.g, self.color.b, px[0]),
            _ => Color::rgba(0, 0, 0, 0),
        };
        if self.rgb_swap {
            core::mem::swap(&mut c.r, &mut c.b);
        }
        c
    }
}

fn source<'a>(input: &In, pixels: &'a [u8], color: Color) -> Source<'a> {
    Source {
        pixels,
        stride: input.format.row_bytes(input.pic_w as usize),
        format: input.format,
        rgb_swap: input.rgb_swap,
        byte_swap: input.byte_swap,
        color,
    }
}

/// The YUV420 block as ARGB8888, chroma taken from the pixel pair's cell.
fn yuv420_block(input: &In) -> Vec<u8> {
    let full = input.yuv.range == YuvRange::Full;
    let bt709 = input.yuv.standard == YuvStandard::Bt709;
    let (ky, yoff, kr, kgu, kgv, kb) = match (full, bt709) {
        (true, true) => (4096, 0, 6450, 767, 1917, 7601),
        (true, false) => (4096, 0, 5743, 1410, 2925, 7258),
        (false, true) => (4768, 16, 7344, 872, 2183, 8651),
        (false, false) => (4768, 16, 6537, 1606, 3330, 8262),
    };
    let (ox, oy, bw, bh) = input.block;
    let stride = input.pic_w as usize * 3 / 2;
    let src = unsafe { core::slice::from_raw_parts(input.ptr, input.len) };
    let mut out = Vec::with_capacity(bw as usize * bh as usize * 4);
    for y in 0..bh {
        let sy = (oy + y) as usize;
        let row = &src[stride * sy..];
        let urow = &src[stride * (sy & !1)..];
        let vrow = &urow[stride..];
        for x in 0..bw {
            let sx = (ox + x) as usize;
            let base = (sx >> 1) * 3;
            let yy = (i32::from(row[base + 1 + (sx & 1)]) - yoff) * ky;
            let u = i32::from(urow[base]) - 128;
            let v = i32::from(vrow[base]) - 128;
            let mut r = clamp8((yy + kr * v + 2048) >> 12);
            let g = clamp8((yy - kgu * u - kgv * v + 2048) >> 12);
            let mut b = clamp8((yy + kb * u + 2048) >> 12);
            if input.rgb_swap {
                core::mem::swap(&mut r, &mut b);
            }
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
    out
}

fn lerp8(a: u8, b: u8, t: f32) -> u8 {
    let v = (f32::from(a) + (f32::from(b) - f32::from(a)) * t).clamp(0.0, 255.0);
    (v + 0.5) as u8
}

fn lerp(c0: Color, c1: Color, t: f32) -> Color {
    Color::rgba(lerp8(c0.r, c1.r, t), lerp8(c0.g, c1.g, t), lerp8(c0.b, c1.b, t), lerp8(c0.a, c1.a, t))
}

fn put(out: &mut [u8], stride: usize, format: ImageFormat, x: u32, y: u32, c: Color) {
    let at = y as usize * stride + x as usize * format.bits() / 8;
    match format {
        ImageFormat::Argb8888 => out[at..at + 4].copy_from_slice(&[c.b, c.g, c.r, c.a]),
        ImageFormat::Bgr888 => out[at..at + 3].copy_from_slice(&[c.b, c.g, c.r]),
        ImageFormat::Rgb565 => {
            let v = narrow5(c.r) << 11 | narrow6(c.g) << 5 | narrow5(c.b);
            out[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }
        _ => {}
    }
}

fn update_alpha(a: u8, alpha: Alpha) -> u8 {
    match alpha {
        Alpha::Keep => a,
        Alpha::Fixed(v) => v,
        Alpha::Scale(v) => ((u32::from(a) * u32::from(v)) >> 8) as u8,
        Alpha::Invert => 255 - a,
    }
}

fn output(plan: &Plan) -> (&mut [u8], usize) {
    let out = &plan.out;
    (unsafe { core::slice::from_raw_parts_mut(out.ptr, out.len) }, out.format.row_bytes(out.pic_w as usize))
}

fn srm(plan: &Plan) {
    let input = &plan.input;
    let yuv;
    let (src, bx, by) = if input.format == ImageFormat::Yuv420 {
        yuv = yuv420_block(input);
        let block = In { pic_w: input.block.2, format: ImageFormat::Argb8888, rgb_swap: false, byte_swap: false, ..*input };
        (source(&block, &yuv, Color::rgb(0, 0, 0)), 0, 0)
    } else {
        let pixels = unsafe { core::slice::from_raw_parts(input.ptr, input.len) };
        (source(input, pixels, Color::rgb(0, 0, 0)), input.block.0, input.block.1)
    };
    let (_, _, bw, bh) = input.block;
    let qx = plan.scale_x.0 * SCALE_FRAG + plan.scale_x.1;
    let qy = plan.scale_y.0 * SCALE_FRAG + plan.scale_y.1;
    let (sx, sy) = (qx as f32 / SCALE_FRAG as f32, qy as f32 / SCALE_FRAG as f32);
    let (sw, sh) = (bw * qx / SCALE_FRAG, bh * qy / SCALE_FRAG);
    let (ow, oh) = if plan.rotation.swaps_axes() { (sh, sw) } else { (sw, sh) };
    let (out, stride) = output(plan);
    let sample = |fx: f32, fy: f32| {
        let (xf, yf) = (floorf(fx), floorf(fy));
        let (wx, wy) = (fx - xf, fy - yf);
        let clamp = |v: i32, max: u32| v.clamp(0, max as i32 - 1) as u32;
        let (x0, x1) = (clamp(xf as i32, bw), clamp(xf as i32 + 1, bw));
        let (y0, y1) = (clamp(yf as i32, bh), clamp(yf as i32 + 1, bh));
        let at = |x: u32, y: u32| src.get(bx + x, by + y);
        lerp(lerp(at(x0, y0), at(x1, y0), wx), lerp(at(x0, y1), at(x1, y1), wx), wy)
    };
    for oy in 0..oh {
        for ox in 0..ow {
            let rx = if plan.mirror_x { ow - 1 - ox } else { ox };
            let ry = if plan.mirror_y { oh - 1 - oy } else { oy };
            let (px, py) = match plan.rotation {
                Rotation::R0 => (rx, ry),
                Rotation::R90 => (sw - 1 - ry, rx),
                Rotation::R180 => (sw - 1 - rx, sh - 1 - ry),
                Rotation::R270 => (ry, sh - 1 - rx),
            };
            let mut c = sample((px as f32 + 0.5) / sx - 0.5, (py as f32 + 0.5) / sy - 0.5);
            c.a = update_alpha(c.a, input.alpha);
            put(out, stride, plan.out.format, plan.out.at.0 + ox, plan.out.at.1 + oy, c);
        }
    }
}

fn blend(plan: &Plan) {
    let (bg, fg) = (&plan.input, &plan.fg);
    let bg_pixels: Vec<u8> = unsafe { core::slice::from_raw_parts(bg.ptr, bg.len) }.to_vec();
    let fg_pixels = unsafe { core::slice::from_raw_parts(fg.ptr, fg.len) };
    let back = source(bg, &bg_pixels, Color::rgb(0, 0, 0));
    let front = source(fg, fg_pixels, plan.fg_color);
    let (out, stride) = output(plan);
    let (w, h) = plan.out.size;
    for y in 0..h {
        for x in 0..w {
            let b = back.get(bg.block.0 + x, bg.block.1 + y);
            let f = front.get(fg.block.0 + x, fg.block.1 + y);
            let fa = u32::from(update_alpha(f.a, fg.alpha));
            let ba = u32::from(update_alpha(b.a, bg.alpha)) * (255 - fa) / 255;
            let sum = fa + ba;
            let c = if sum == 0 {
                Color::rgba(0, 0, 0, 0)
            } else {
                let mix = |f: u8, b: u8| ((u32::from(f) * fa + u32::from(b) * ba) / sum) as u8;
                Color::rgba(mix(f.r, b.r), mix(f.g, b.g), mix(f.b, b.b), sum.min(255) as u8)
            };
            put(out, stride, plan.out.format, plan.out.at.0 + x, plan.out.at.1 + y, c);
        }
    }
}

fn fill(plan: &Plan) {
    let (out, stride) = output(plan);
    let (w, h) = plan.out.size;
    for y in 0..h {
        for x in 0..w {
            put(out, stride, plan.out.format, plan.out.at.0 + x, plan.out.at.1 + y, plan.color);
        }
    }
}
