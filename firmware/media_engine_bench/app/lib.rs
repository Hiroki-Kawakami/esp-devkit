/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unsafe_op_in_unsafe_fn)]
#[allow(clippy::all)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
mod idf_ppa;

use alloc::vec::Vec;
use core::alloc::{Allocator, Layout};
use core::ffi::CStr;
use core::time::Duration;

use devkit_gfx::{Color, Image, ImageFormat, Rect, Rotation, Size, Target};
use idf::heap::Caps;
use idf::log::{error, info};
use idf::time::Instant;
use media_engine::{Background, Blend, Fill, Slot, Srm, ppa};

const TAG: &CStr = c"media_engine_bench";
const SRC_W: usize = 1280;
const SRC_H: usize = 720;
const STRIP_ROWS: usize = 16;
const REPS: usize = 3;
const STABILITY_FRAMES: usize = 500;
const FNV_SEED: u32 = 0x811c_9dc5;

fn fnv(data: &[u8]) -> u32 {
    data.iter().fold(FNV_SEED, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

fn buffer(len: usize) -> &'static mut [u8] {
    let len = len.next_multiple_of(64);
    let layout = Layout::from_size_align(len, 64).unwrap();
    let block = Caps::SPIRAM.allocate_zeroed(layout).expect("PSRAM");
    unsafe { core::slice::from_raw_parts_mut(block.as_ptr().cast(), len) }
}

fn fill_source(src: &mut [u8], format: ImageFormat) {
    let bytes = format.bits() / 8;
    for y in 0..SRC_H {
        for x in 0..SRC_W {
            let r = ((x * 255 / SRC_W) ^ (y & 0x1f)) as u8;
            let g = (y * 255 / SRC_H) as u8;
            let b = ((x + y) * 3) as u8;
            let at = (y * SRC_W + x) * bytes;
            if bytes == 2 {
                let v = u16::from(r >> 3) << 11 | u16::from(g >> 2) << 5 | u16::from(b >> 3);
                src[at..at + 2].copy_from_slice(&v.to_le_bytes());
            } else {
                src[at..at + 3].copy_from_slice(&[b, g, r]);
            }
        }
    }
}

fn idf_mode(format: ImageFormat) -> idf_ppa::ColorMode {
    match format {
        ImageFormat::Rgb565 => idf_ppa::ColorMode::Rgb565,
        ImageFormat::Bgr888 => idf_ppa::ColorMode::Rgb888,
        _ => idf_ppa::ColorMode::Argb8888,
    }
}

fn idf_rotation(r: Rotation) -> idf_ppa::Rotation {
    match r {
        Rotation::R0 => idf_ppa::Rotation::R0,
        Rotation::R90 => idf_ppa::Rotation::R90,
        Rotation::R180 => idf_ppa::Rotation::R180,
        Rotation::R270 => idf_ppa::Rotation::R270,
    }
}

struct Case {
    name: &'static str,
    block: Rect,
    rotation: Rotation,
    scale: f32,
}

fn out_size(case: &Case) -> (usize, usize) {
    let w = (case.block.w as f32 * case.scale) as usize;
    let h = (case.block.h as f32 * case.scale) as usize;
    if case.rotation.swaps_axes() { (h, w) } else { (w, h) }
}

fn run_idf(srm: &mut idf_ppa::Srm, src: &[u8], src_format: ImageFormat, case: &Case, dst: &mut [u8], (w, h): (usize, usize)) {
    let op = idf_ppa::SrmOp {
        src: src.as_ptr(),
        src_width: SRC_W as u32,
        src_height: SRC_H as u32,
        src_x: case.block.x as u32,
        src_y: case.block.y as u32,
        block_width: case.block.w as u32,
        block_height: case.block.h as u32,
        src_format: idf_mode(src_format),
        dst: dst.as_mut_ptr(),
        dst_size: dst.len() as u32,
        dst_width: w as u32,
        dst_height: h as u32,
        dst_x: 0,
        dst_y: 0,
        format: idf_ppa::ColorMode::Rgb565,
        rotation: idf_rotation(case.rotation),
        scale: case.scale,
    };
    if let Err(err) = unsafe { srm.start(&op) } {
        error!(TAG, "{}: IDF: {err}", case.name);
        return;
    }
    srm.wait();
}

fn run_engine(src: &[u8], src_format: ImageFormat, case: &Case, dst: &mut [u8], (w, h): (usize, usize)) -> idf::Result<()> {
    let image = Image::from_pixels(src_format, SRC_W as u16, SRC_H as u16, SRC_W, src);
    let mut op = Srm::new(image);
    op.src_rect = case.block;
    op.rotation = case.rotation;
    op.scale_x = case.scale;
    op.scale_y = case.scale;
    let mut target = Target::new(dst, ImageFormat::Rgb565, Size::new(w as u16, h as u16), w).ok_or(idf::Error::INVALID_ARG)?;
    ppa()?.srm(&op, &mut target, &mut Slot::new())?.wait()
}

fn best(mut f: impl FnMut()) -> Duration {
    (0..REPS)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap_or_default()
}

fn compare_srm(srm: &mut idf_ppa::Srm, src: &[u8], src_format: ImageFormat, dst: &mut [u8]) {
    let full = Rect::new(0, 0, SRC_W as i16, SRC_H as i16);
    let cases = [
        Case { name: "copy", block: full, rotation: Rotation::R0, scale: 1.0 },
        Case { name: "turn 90", block: full, rotation: Rotation::R90, scale: 1.0 },
        Case { name: "turn 270 x0.5", block: full, rotation: Rotation::R270, scale: 0.5 },
        Case { name: "block x1.5", block: Rect::new(320, 180, 640, 360), rotation: Rotation::R0, scale: 1.5 },
        Case { name: "turn 180 x0.75", block: full, rotation: Rotation::R180, scale: 0.75 },
    ];
    for case in &cases {
        let size = out_size(case);
        let len = size.0 * size.1 * 2;
        dst[..len].fill(0);
        let t_idf = best(|| run_idf(srm, src, src_format, case, &mut dst[..len], size));
        let a = fnv(&dst[..len]);
        dst[..len].fill(0);
        let mut result = Ok(());
        let t_me = best(|| result = run_engine(src, src_format, case, &mut dst[..len], size));
        let b = fnv(&dst[..len]);
        if let Err(err) = result {
            error!(TAG, "{:?} {}: media_engine: {err}", src_format, case.name);
        }
        info!(
            TAG,
            "{:?} {:<16} idf {:>6} us  media_engine {:>6} us  {}",
            src_format,
            case.name,
            t_idf.as_micros(),
            t_me.as_micros(),
            if a == b { "same" } else { "DIFFERENT" }
        );
    }
}

fn strips_idf(srm: &mut idf_ppa::Srm, src: &[u8], dst: &mut [u8]) {
    for s in 0..SRC_H / STRIP_ROWS {
        let op = idf_ppa::SrmOp {
            src: unsafe { src.as_ptr().add(s * STRIP_ROWS * SRC_W * 2) },
            src_width: SRC_W as u32,
            src_height: STRIP_ROWS as u32,
            src_x: 0,
            src_y: 0,
            block_width: SRC_W as u32,
            block_height: STRIP_ROWS as u32,
            src_format: idf_ppa::ColorMode::Rgb565,
            dst: dst.as_mut_ptr(),
            dst_size: dst.len() as u32,
            dst_width: SRC_H as u32,
            dst_height: SRC_W as u32,
            dst_x: (s * STRIP_ROWS) as u32,
            dst_y: 0,
            format: idf_ppa::ColorMode::Rgb565,
            rotation: idf_ppa::Rotation::R90,
            scale: 1.0,
        };
        if unsafe { srm.start(&op) }.is_err() {
            return;
        }
        srm.wait();
    }
}

/// Every strip queued before the first wait when `queued`, one at a time
/// otherwise. Each strip gets a target of its own over its rows.
fn strips_engine(src: &[u8], dst: &mut [u8], queued: bool) -> idf::Result<()> {
    let ppa = ppa()?;
    let image = Image::from_pixels(ImageFormat::Rgb565, SRC_W as u16, SRC_H as u16, SRC_W, src);
    let ptr = core::ptr::NonNull::new(dst.as_mut_ptr()).ok_or(idf::Error::INVALID_ARG)?;
    let size = Size::new(SRC_H as u16, SRC_W as u16);
    let mut targets = Vec::new();
    for s in 0..SRC_H / STRIP_ROWS {
        let rows = Rect::new(0, (s * STRIP_ROWS) as i16, SRC_W as i16, STRIP_ROWS as i16);
        let target = unsafe { Target::from_raw(ptr, dst.len(), ImageFormat::Rgb565, size, SRC_H) }
            .ok_or(idf::Error::INVALID_ARG)?
            .rotated(Rotation::R90)
            .crop(rows)
            .ok_or(idf::Error::INVALID_ARG)?;
        targets.push(target);
    }
    let mut slots: Vec<Slot> = (0..targets.len()).map(|_| Slot::new()).collect();
    let mut jobs = Vec::new();
    for (s, (target, slot)) in targets.iter_mut().zip(&mut slots).enumerate() {
        let mut op = Srm::new(image);
        op.src_rect = Rect::new(0, (s * STRIP_ROWS) as i16, SRC_W as i16, STRIP_ROWS as i16);
        op.at = (0, (s * STRIP_ROWS) as i16);
        let job = ppa.srm(&op, target, slot)?;
        if queued {
            jobs.push(job);
        } else {
            job.wait()?;
        }
    }
    for job in jobs {
        job.wait()?;
    }
    Ok(())
}

fn compare_strips(srm: &mut idf_ppa::Srm, src: &[u8], dst: &mut [u8]) {
    let len = SRC_W * SRC_H * 2;
    dst[..len].fill(0);
    let t_idf = best(|| strips_idf(srm, src, &mut dst[..len]));
    let a = fnv(&dst[..len]);
    for queued in [false, true] {
        dst[..len].fill(0);
        let mut result = Ok(());
        let t_me = best(|| result = strips_engine(src, &mut dst[..len], queued));
        if let Err(err) = result {
            error!(TAG, "strips: {err}");
        }
        let b = fnv(&dst[..len]);
        info!(
            TAG,
            "{} 16-row strips turned  idf {:>6} us  media_engine{} {:>6} us  {}",
            SRC_H / STRIP_ROWS,
            t_idf.as_micros(),
            if queued { " queued" } else { "" },
            t_me.as_micros(),
            if a == b { "same" } else { "DIFFERENT" }
        );
    }
    let mut changed = 0;
    // The simulator draws on the CPU, far too slowly for the full run.
    let frames = if cfg!(target_os = "none") { STABILITY_FRAMES } else { 2 };
    for _ in 0..frames {
        if strips_engine(src, &mut dst[..len], true).is_err() || fnv(&dst[..len]) != a {
            changed += 1;
        }
    }
    info!(TAG, "{frames} queued frames, {changed} different");
}

/// The shortest of the times `f` reports.
fn shortest(mut f: impl FnMut() -> Duration) -> Duration {
    (0..REPS).map(|_| f()).min().unwrap_or_default()
}

fn compare_blend(src565: &mut [u8], dst: &mut [u8]) {
    const W: usize = 256;
    let fg = buffer(W * W * 4);
    for y in 0..W {
        for x in 0..W {
            fg[(y * W + x) * 4..][..4].copy_from_slice(&[(x * 7) as u8, (y * 3) as u8, (x ^ y) as u8, (x + y) as u8]);
        }
    }
    let len = SRC_W * SRC_H * 2;
    let at = (100, 60);
    dst[..len].copy_from_slice(&src565[..len]);
    let mut blend = match idf_ppa::Blend::new() {
        Ok(b) => b,
        Err(err) => return error!(TAG, "IDF blend: {err}"),
    };
    let op = idf_ppa::BlendOp {
        foreground: fg.as_ptr(),
        width: W as u32,
        height: W as u32,
        picture: dst.as_mut_ptr(),
        picture_size: len as u32,
        picture_width: SRC_W as u32,
        picture_height: SRC_H as u32,
        x: at.0 as u32,
        y: at.1 as u32,
        format: idf_ppa::ColorMode::Rgb565,
    };
    let mut ok = Ok(false);
    let t_idf = shortest(|| {
        dst[..len].copy_from_slice(&src565[..len]);
        let t = Instant::now();
        ok = unsafe { blend.run(&op, Duration::from_secs(1)) };
        t.elapsed()
    });
    let a = fnv(&dst[..len]);
    let mut result = Ok(());
    let t_me = shortest(|| {
        dst[..len].copy_from_slice(&src565[..len]);
        let image = Image::from_pixels(ImageFormat::Argb8888, W as u16, W as u16, W, fg);
        let Some(mut target) = Target::new(&mut dst[..len], ImageFormat::Rgb565, Size::new(SRC_W as u16, SRC_H as u16), SRC_W) else {
            result = Err(idf::Error::INVALID_ARG);
            return Duration::ZERO;
        };
        let mut op = Blend::new(image, at);
        op.bg = Background::Target;
        let mut slot = Slot::new();
        let t = Instant::now();
        result = ppa().and_then(|ppa| ppa.blend(&op, &mut target, &mut slot)?.wait());
        t.elapsed()
    });
    let b = fnv(&dst[..len]);
    if ok.is_err() || result.is_err() {
        error!(TAG, "blend: idf {ok:?} media_engine {result:?}");
    }
    info!(
        TAG,
        "blend 256x256 ARGB over RGB565  idf {:>6} us  media_engine {:>6} us  {}",
        t_idf.as_micros(),
        t_me.as_micros(),
        if a == b { "same" } else { "DIFFERENT" }
    );
}

fn check_fill(dst: &mut [u8]) {
    let (w, h) = (64usize, 48usize);
    let len = w * h * 2;
    dst[..len].fill(0);
    let rect = Rect::new(5, 7, 20, 11);
    let result = (|| {
        let mut target =
            Target::new(&mut dst[..len], ImageFormat::Rgb565, Size::new(w as u16, h as u16), w).ok_or(idf::Error::INVALID_ARG)?;
        ppa()?.fill(&Fill { rect, color: Color::rgb(255, 0, 0) }, &mut target, &mut Slot::new())?.wait()
    })();
    let mut wrong = 0;
    for y in 0..h {
        for x in 0..w {
            let inside = (5..25).contains(&x) && (7..18).contains(&y);
            let px = u16::from_le_bytes([dst[(y * w + x) * 2], dst[(y * w + x) * 2 + 1]]);
            if px != if inside { 0xf800 } else { 0 } {
                wrong += 1;
            }
        }
    }
    info!(TAG, "fill 20x11 RGB565: {result:?}, {wrong} pixels wrong");
}

pub fn run() {
    let src565 = buffer(SRC_W * SRC_H * 2);
    let src888 = buffer(SRC_W * SRC_H * 3);
    let dst = buffer(SRC_W * SRC_H * 2);
    fill_source(src565, ImageFormat::Rgb565);
    fill_source(src888, ImageFormat::Bgr888);
    let mut srm = match idf_ppa::Srm::new() {
        Ok(s) => s,
        Err(err) => return error!(TAG, "IDF SRM: {err}"),
    };
    info!(TAG, "media_engine against IDF's PPA driver, source {SRC_W}x{SRC_H}, best of {REPS}");
    compare_srm(&mut srm, src565, ImageFormat::Rgb565, dst);
    compare_srm(&mut srm, src888, ImageFormat::Bgr888, dst);
    compare_strips(&mut srm, src565, dst);
    compare_blend(src565, dst);
    check_fill(dst);
    info!(TAG, "done");
}
