/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::alloc::{Allocator, Layout};
use core::time::Duration;

use devkit_gfx::{ImageFormat, Rect, Rotation, Size, Target};
use devkit_image::{JpegEncoder, JpegOptions, Subsampling, encode_buffer};
use idf::heap::Caps;
use idf::log::{error, info};
use idf::time::Instant;
use media_engine::{DecodeConfig, Decoder, Header, Transform, Yuv, YuvRange, YuvStandard};

use crate::{FNV_SEED, REPS, TAG, buffer};

const STRIP_ROWS: usize = 32;
const RENDER_FRAMES: usize = 100;
const FORMATS: [ImageFormat; 3] = [ImageFormat::Rgb565, ImageFormat::Bgr888, ImageFormat::Rgb888];

struct Picture {
    name: &'static str,
    data: Vec<u8>,
    header: Header,
}

fn fnv_from(h: u32, data: &[u8]) -> u32 {
    data.iter().fold(h, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

fn picture(name: &'static str, w: usize, h: usize, subsampling: Subsampling) -> Option<Picture> {
    let mut src = alloc::vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let r = ((x * 255 / w) ^ (y & 0x1f)) as u8;
            let g = (y * 255 / h) as u8;
            let b = ((x + y) * 3) as u8 ^ ((x * y) >> 6) as u8;
            src[(y * w + x) * 3..][..3].copy_from_slice(&[r, g, b]);
        }
    }
    let mut data = Vec::new();
    let sink = |chunk: &[u8]| {
        data.extend_from_slice(chunk);
        Ok(())
    };
    let options = JpegOptions { quality: 85, subsampling, ..Default::default() };
    let encoded = JpegEncoder::new(w as u16, h as u16, ImageFormat::Rgb888, &options, sink).and_then(|e| encode_buffer(e, &src, 0));
    if let Err(err) = encoded {
        error!(TAG, "{name}: encode: {err:?}");
        return None;
    }
    let header = match Header::parse(&data) {
        Ok(h) => h,
        Err(err) => {
            error!(TAG, "{name}: header: {err}");
            return None;
        }
    };
    Some(Picture { name, data, header })
}

fn picture_bytes(p: &Picture, format: ImageFormat) -> usize {
    format.row_bytes(usize::from(p.header.padded_width)) * usize::from(p.header.padded_height)
}

fn strip_buffers(len: usize) -> Option<[&'static mut [u8]; 2]> {
    let layout = Layout::from_size_align(len, 64).ok()?;
    let one = || {
        let block = (Caps::INTERNAL | Caps::DMA).allocate_zeroed(layout).ok()?;
        Some(unsafe { core::slice::from_raw_parts_mut(block.as_ptr().cast::<u8>(), len) })
    };
    Some([one()?, one()?])
}

fn best(mut f: impl FnMut() -> idf::Result<()>) -> (Duration, idf::Result<()>) {
    let mut result = Ok(());
    let t = (0..REPS)
        .map(|_| {
            let t = Instant::now();
            result = result.and(f());
            t.elapsed()
        })
        .min()
        .unwrap_or_default();
    (t, result)
}

fn config(format: ImageFormat) -> DecodeConfig {
    DecodeConfig { format, yuv: Yuv { range: YuvRange::Full, standard: YuvStandard::Bt601 } }
}

fn whole(dec: &mut Decoder, p: &Picture, format: ImageFormat, out: &mut [u8]) -> idf::Result<()> {
    let size = Size::new(p.header.padded_width, p.header.padded_height);
    let mut target = Target::new(out, format, size, usize::from(p.header.padded_width)).ok_or(idf::Error::INVALID_ARG)?;
    dec.decode(&p.data, &mut target)?.wait()
}

/// The checksum of the strips laid end to end.
fn strips(dec: &mut Decoder, p: &Picture, buffers: &mut [&mut [u8]]) -> idf::Result<u32> {
    let mut strips = dec.strips(&p.data, buffers)?;
    let mut h = FNV_SEED;
    while let Some(strip) = strips.next_blocking()? {
        h = fnv_from(h, &strip.pixels()[..usize::from(strip.padded_rows()) * strip.stride_bytes()]);
    }
    Ok(h)
}

fn released(dec: &mut Decoder, p: &Picture, buffers: &mut [&mut [u8]]) -> idf::Result<()> {
    let mut strips = dec.strips(&p.data, buffers)?;
    while strips.next_blocking()?.is_some() {}
    Ok(())
}

#[cfg(target_os = "none")]
fn reference(p: &Picture, format: ImageFormat, out: &mut [u8], strip_bufs: [*mut u8; 2], strip_len: usize) -> Option<(u32, Duration, Duration)> {
    let len = picture_bytes(p, format);
    let mut dec = match crate::c_jpeg::Decoder::new(format, strip_bufs, strip_len) {
        Ok(d) => d,
        Err(err) => {
            error!(TAG, "{} {format:?}: C decoder: {err}", p.name);
            return None;
        }
    };
    out[..len].fill(0);
    let (t_whole, a) = best(|| dec.decode(&p.data, &mut out[..len.next_multiple_of(64)]));
    let (t_strips, b) = best(|| dec.strips(&p.data).map(|_| ()));
    if a.is_err() || b.is_err() {
        error!(TAG, "{} {format:?}: C: whole {a:?} strips {b:?}", p.name);
        return None;
    }
    Some((fnv_from(FNV_SEED, &out[..len]), t_whole, t_strips))
}

#[cfg(not(target_os = "none"))]
fn reference(_: &Picture, _: ImageFormat, _: &mut [u8], _: [*mut u8; 2], _: usize) -> Option<(u32, Duration, Duration)> {
    None
}

fn strip_len(p: &Picture, format: ImageFormat) -> usize {
    format.row_bytes(usize::from(p.header.padded_width)) * STRIP_ROWS
}

fn compare(p: &Picture, format: ImageFormat, reference: Option<(u32, Duration, Duration)>, out: &mut [u8], bufs: &mut [&'static mut [u8]; 2]) {
    let len = picture_bytes(p, format);
    let strip_len = strip_len(p, format);

    let mut dec = match Decoder::new(&config(format)) {
        Ok(d) => d,
        Err(err) => return error!(TAG, "{} {format:?}: media_engine: {err}", p.name),
    };
    out[..len].fill(0);
    let (t_whole, a) = best(|| whole(&mut dec, p, format, &mut out[..len]));
    let crc_whole = fnv_from(FNV_SEED, &out[..len]);
    let [a_buf, b_buf] = bufs;
    let mut lanes = [&mut a_buf[..strip_len], &mut b_buf[..strip_len]];
    let crc_strips = strips(&mut dec, p, &mut lanes);
    let (t_strips, b) = best(|| released(&mut dec, p, &mut lanes));
    if a.is_err() || b.is_err() || crc_strips.is_err() {
        return error!(TAG, "{} {format:?}: media_engine: whole {a:?} strips {b:?} {crc_strips:?}", p.name);
    }
    let strips_same = crc_strips == Ok(crc_whole);
    match reference {
        Some((crc_c, c_whole, c_strips)) => info!(
            TAG,
            "{:<14} {:<7?} whole C {:>6} us  media_engine {:>6} us  {}  strips C {:>6} us  media_engine {:>6} us  {}",
            p.name,
            format,
            c_whole.as_micros(),
            t_whole.as_micros(),
            if crc_c == crc_whole { "same" } else { "DIFFERENT" },
            c_strips.as_micros(),
            t_strips.as_micros(),
            if strips_same { "same" } else { "DIFFERENT" }
        ),
        None => info!(
            TAG,
            "{:<14} {:<7?} whole {:>6} us  strips {:>6} us  strips vs whole {}",
            p.name,
            format,
            t_whole.as_micros(),
            t_strips.as_micros(),
            if strips_same { "same" } else { "DIFFERENT" }
        ),
    }
}

/// One render of the 1280x720 picture into an RGB565 picture of `out`.
struct RenderCase {
    name: &'static str,
    rotation: Rotation,
    scale: f32,
    mirror_x: bool,
    crop: Option<Rect>,
    at: (i16, i16),
    clip: Option<Rect>,
    out: (u16, u16),
}

const RENDERS: [RenderCase; 4] = [
    RenderCase { name: "turn 90", rotation: Rotation::R90, scale: 1.0, mirror_x: false, crop: None, at: (0, 0), clip: None, out: (720, 1280) },
    RenderCase {
        name: "turn 270 x0.5 mirror",
        rotation: Rotation::R270,
        scale: 0.5,
        mirror_x: true,
        crop: None,
        at: (100, 200),
        clip: None,
        out: (720, 1280),
    },
    RenderCase {
        name: "x0.75 clipped",
        rotation: Rotation::R0,
        scale: 0.75,
        mirror_x: false,
        crop: None,
        at: (100, 50),
        clip: Some(Rect::new(300, 200, 500, 300)),
        out: (1280, 720),
    },
    RenderCase {
        name: "crop turn 180",
        rotation: Rotation::R180,
        scale: 1.0,
        mirror_x: false,
        crop: Some(Rect::new(100, 64, 800, 480)),
        at: (0, 0),
        clip: None,
        out: (1280, 720),
    },
];

fn out_len(case: &RenderCase) -> usize {
    usize::from(case.out.0) * usize::from(case.out.1) * 2
}

#[cfg(target_os = "none")]
fn render_reference(p: &Picture, case: &RenderCase, out: &mut [u8], strip_bufs: [*mut u8; 2], strip_len: usize) -> Option<(u32, Duration)> {
    let mut pipe = match crate::c_jpeg::Pipeline::new(strip_bufs, strip_len) {
        Ok(pipe) => pipe,
        Err(err) => {
            error!(TAG, "{}: C pipeline: {err}", case.name);
            return None;
        }
    };
    let quarter_turns = match case.rotation {
        Rotation::R0 => 0,
        Rotation::R90 => 1,
        Rotation::R180 => 2,
        Rotation::R270 => 3,
    };
    let rect = |r: Option<Rect>| r.map(|r| (r.x as u32, r.y as u32, r.w as u32, r.h as u32));
    let c = crate::c_jpeg::Case {
        quarter_turns,
        scale: case.scale,
        mirror_x: case.mirror_x,
        crop: rect(case.crop),
        offset: (case.at.0 as u32, case.at.1 as u32),
        clip: rect(case.clip),
    };
    let len = out_len(case);
    let size = (u32::from(case.out.0), u32::from(case.out.1));
    out[..len].fill(0);
    // The C pipeline drops the caches over its whole output without writing them back.
    let _ = unsafe { idf::cache::msync(out.as_ptr(), len, idf::cache::Flags::C2M) };
    let (t, result) = best(|| pipe.process(&p.data, &mut out[..len], size, &c));
    if let Err(err) = result {
        error!(TAG, "{}: C pipeline: {err}", case.name);
        return None;
    }
    Some((fnv_from(FNV_SEED, &out[..len]), t))
}


#[cfg(not(target_os = "none"))]
fn render_reference(_: &Picture, _: &RenderCase, _: &mut [u8], _: [*mut u8; 2], _: usize) -> Option<(u32, Duration)> {
    None
}

fn render(dec: &mut Decoder, p: &Picture, case: &RenderCase, out: &mut [u8], buffers: &mut [&mut [u8]]) -> idf::Result<()> {
    let size = Size::new(case.out.0, case.out.1);
    let mut target = Target::new(out, ImageFormat::Rgb565, size, usize::from(case.out.0)).ok_or(idf::Error::INVALID_ARG)?;
    if let Some(clip) = case.clip {
        target = target.crop(clip).ok_or(idf::Error::INVALID_ARG)?;
    }
    let t = Transform {
        src_rect: case.crop,
        at: case.at,
        rotation: case.rotation,
        scale_x: case.scale,
        scale_y: case.scale,
        mirror_x: case.mirror_x,
        ..Transform::new()
    };
    dec.render(&p.data, buffers, &t, &mut target)?.wait()
}

fn compare_render(p: &Picture, case: &RenderCase, reference: Option<(u32, Duration)>, out: &mut [u8], bufs: &mut [&'static mut [u8]; 2]) {
    let len = out_len(case);
    let strip_len = strip_len(p, ImageFormat::Rgb565);
    let mut dec = match Decoder::new(&config(ImageFormat::Rgb565)) {
        Ok(d) => d,
        Err(err) => return error!(TAG, "{}: media_engine: {err}", case.name),
    };
    let [a_buf, b_buf] = bufs;
    let mut lanes = [&mut a_buf[..strip_len], &mut b_buf[..strip_len]];
    out[..len].fill(0);
    let (t, result) = best(|| render(&mut dec, p, case, &mut out[..len], &mut lanes));
    if let Err(err) = result {
        return error!(TAG, "{}: media_engine: {err}", case.name);
    }
    let crc = fnv_from(FNV_SEED, &out[..len]);
    let frames = if cfg!(target_os = "none") { RENDER_FRAMES } else { 2 };
    let changed = (0..frames).filter(|_| render(&mut dec, p, case, &mut out[..len], &mut lanes).is_err() || fnv_from(FNV_SEED, &out[..len]) != crc).count();
    match reference {
        Some((crc_c, t_c)) => info!(
            TAG,
            "render {:<22} C {:>6} us  media_engine {:>6} us  {}, {frames} more frames {changed} different",
            case.name,
            t_c.as_micros(),
            t.as_micros(),
            if crc_c == crc { "same" } else { "DIFFERENT" }
        ),
        None => info!(TAG, "render {:<22} {:>6} us, {frames} more frames {changed} different", case.name, t.as_micros()),
    }
}

pub fn run() {
    let pictures = [
        picture("1280x720 4:2:2", 1280, 720, Subsampling::S422),
        picture("800x600 4:2:2", 800, 600, Subsampling::S422),
        picture("640x480 4:2:0", 640, 480, Subsampling::S420),
    ];
    let out = buffer(1280 * 720 * 3);
    let Some(mut bufs) = strip_buffers(1280 * 3 * STRIP_ROWS) else {
        return error!(TAG, "no internal memory for strips");
    };
    info!(TAG, "JPEG decode, {STRIP_ROWS}-row strips in two internal buffers, best of {REPS}");
    let pictures: Vec<Picture> = pictures.into_iter().flatten().collect();
    // Both drivers take the codec's interrupt, and media_engine keeps it once taken.
    let mut references = Vec::new();
    for p in &pictures {
        for format in FORMATS {
            let ptrs = [bufs[0].as_mut_ptr(), bufs[1].as_mut_ptr()];
            references.push(reference(p, format, out, ptrs, strip_len(p, format)));
        }
    }
    let mut render_references = Vec::new();
    if let Some(p) = pictures.first() {
        for case in &RENDERS {
            let ptrs = [bufs[0].as_mut_ptr(), bufs[1].as_mut_ptr()];
            render_references.push(render_reference(p, case, out, ptrs, strip_len(p, ImageFormat::Rgb565)));
        }
    }
    let mut references = references.into_iter();
    for p in &pictures {
        for format in FORMATS {
            compare(p, format, references.next().flatten(), out, &mut bufs);
        }
    }
    if let Some(p) = pictures.first() {
        info!(TAG, "{} into RGB565 through {STRIP_ROWS}-row RGB565 strips", p.name);
        for (case, reference) in RENDERS.iter().zip(render_references) {
            compare_render(p, case, reference, out, &mut bufs);
        }
    }
}
