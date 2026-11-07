/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::alloc::{Allocator, Layout};
use core::time::Duration;

use devkit_gfx::{ImageFormat, Size, Target};
use devkit_image::{JpegEncoder, JpegOptions, Subsampling, encode_buffer};
use idf::heap::Caps;
use idf::log::{error, info};
use idf::time::Instant;
use media_engine::{DecodeConfig, Decoder, Header, Yuv, YuvRange, YuvStandard};

use crate::{FNV_SEED, REPS, TAG, buffer};

const STRIP_ROWS: usize = 32;
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
    let mut references = references.into_iter();
    for p in &pictures {
        for format in FORMATS {
            compare(p, format, references.next().flatten(), out, &mut bufs);
        }
    }
}
