/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

mod ffi {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_void};
use core::mem;
use core::ptr;
use core::slice;

use devkit_image::{
    DecodeOptions, Decoder, Format, ImageInfo, JpegEncoder, JpegOptions, Orientation, PixelFormat, ResizeOptions,
    SliceStream, Subsampling, default_caps, encode_buffer, internal_caps, jpeg_buffer_upper_bound, resize_buffer,
    resize_decoder,
};
use idf::heap::{self, Caps};
use idf::log::info;
use idf::time::Instant;

const TAG: &CStr = c"bench";
const SRC_W: usize = 1920;
const SRC_H: usize = 1080;
const REPS: usize = 3;
const QUALITIES: [u8; 2] = [85, 95];

const PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/png.bin"));
const PROGRESSIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/progressive.bin"));
const WEBP_LOSSY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/webp_lossy.bin"));
const WEBP_LOSSLESS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/webp_lossless.bin"));

type Block = Vec<u8, Caps>;

fn block(len: usize, caps: Caps) -> Block {
    let mut v = heap::try_vec(len, caps).expect("bench buffer");
    v.resize(len, 0);
    v
}

fn psram(len: usize) -> Block {
    block(len, default_caps())
}

const FNV_SEED: u32 = 2166136261;

fn fnv(hash: u32, data: &[u8]) -> u32 {
    data.iter().fold(hash, |h, &b| (h ^ u32::from(b)).wrapping_mul(16777619))
}

// Shaped like the picture gen_assets.py draws: a low-frequency colour field,
// near-Nyquist texture and grain. Integer only, so it costs the same on every
// target; the texture is what gives the entropy stages realistic work.
fn fill_source(p: &mut [u8]) {
    let mut table = [0i32; 256];
    for i in 0..128 {
        table[i] = (i * (128 - i)) as i32;
        table[i + 128] = -((i * (128 - i)) as i32);
    }
    let isin = |a: i32| table[(a & 255) as usize];
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    let mut s = 2463534242u32;
    let (w, h) = (SRC_W as i32, SRC_H as i32);
    let mut px = p.chunks_exact_mut(3);
    for y in 0..h {
        let ay = y * 2048 / h;
        for x in 0..w {
            let ax = x * 2304 / w;
            let lo = (isin(ax) + isin(ay + 53) + isin((ax + ay) >> 1) / 2) >> 6;
            let tex = (((isin(x * 85) * isin(y * 69)) >> 12) + ((isin((x + y) * 46) * 3) >> 2)) * 5 >> 12;
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let n = (((s >> 8) & 0x3F) as i32 - 32) / 8;
            let out = px.next().unwrap();
            out[0] = clamp(128 + lo + x * 60 / w + tex + n);
            out[1] = clamp(128 + (isin(lo * 2) >> 6) + y * 50 / h + tex + n);
            out[2] = clamp(128 + (isin(lo + 64) >> 6) - tex + n);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Run {
    us: u64,
    hash: u32,
}

fn timed(reps: usize, mut f: impl FnMut(bool) -> Result<u32, String>) -> Result<Run, String> {
    let mut run = Run { us: u64::MAX, hash: 0 };
    for i in 0..reps {
        let start = Instant::now();
        let hash = f(i == 0)?;
        run.us = run.us.min(start.elapsed().as_micros() as u64);
        if i == 0 {
            run.hash = hash;
        }
    }
    Ok(run)
}

fn report(name: &str, c: Result<Run, String>, rust: Result<Run, String>, pixels: usize) {
    match (c, rust) {
        (Ok(c), Ok(rust)) => {
            let verdict = if c.hash == rust.hash { "match" } else { "MISMATCH" };
            info!(
                TAG,
                "{name:<46} C {:>9.2} ms  Rust {:>9.2} ms  x{:.3}  {:.2} Mpx/s  {:08x} {verdict}",
                c.us as f64 / 1000.0,
                rust.us as f64 / 1000.0,
                rust.us as f64 / c.us as f64,
                pixels as f64 / rust.us as f64,
                rust.hash,
            );
        }
        (c, rust) => {
            info!(TAG, "{name:<46} FAILED C {:?} Rust {:?}", c.map(|r| r.us), rust.map(|r| r.us));
        }
    }
}

fn c_format(format: PixelFormat) -> ffi::imgf_pixfmt_t {
    match format {
        PixelFormat::Gray8 => ffi::imgf_pixfmt_t_IMGF_PIX_GRAY8,
        PixelFormat::Rgb888 => ffi::imgf_pixfmt_t_IMGF_PIX_RGB888,
        PixelFormat::Rgb565 => ffi::imgf_pixfmt_t_IMGF_PIX_RGB565,
        PixelFormat::Bgr888 => ffi::imgf_pixfmt_t_IMGF_PIX_BGR888,
    }
}

fn format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Gray8 => "GRAY8",
        PixelFormat::Rgb888 => "RGB888",
        PixelFormat::Rgb565 => "RGB565",
        PixelFormat::Bgr888 => "BGR888",
    }
}

fn c_decoder(format: Format) -> *mut ffi::imgf_decoder_t {
    unsafe {
        match format {
            Format::Jpeg => ffi::imgf_jpegd_create(),
            Format::Png => ffi::imgf_pngd_create(),
            Format::Webp => ffi::imgf_webpd_create(),
            _ => ptr::null_mut(),
        }
    }
}

struct COpen {
    d: *mut ffi::imgf_decoder_t,
    _state: ffi::imgf_buffer_source_t,
}

fn c_open(format: Format, data: &[u8], tw: u16, th: u16, state: &mut ffi::imgf_buffer_source_t) -> Result<*mut ffi::imgf_decoder_t, String> {
    let d = c_decoder(format);
    let mut opts: ffi::imgf_decode_opts_t = unsafe { mem::zeroed() };
    opts.target_w = tw;
    opts.target_h = th;
    let stream = unsafe { ffi::imgf_stream_from_buffer(state, data.as_ptr().cast(), data.len()) };
    let err = unsafe { ffi::imgf_decoder_open(d, stream, &opts) };
    if err != ffi::IMGF_OK as i32 {
        unsafe { ffi::imgf_decoder_destroy(d) };
        return Err(format!("open {err}"));
    }
    Ok(d)
}

fn decode_c(format: Format, data: &[u8], tw: u16, th: u16, row: &mut [u8], keep: bool) -> Result<u32, String> {
    let mut state = unsafe { mem::zeroed() };
    let d = c_open(format, data, tw, th, &mut state)?;
    let open = COpen { d, _state: state };
    let bytes = unsafe {
        usize::from(ffi::imgf_decoder_width(d)) * ffi::imgf_pixfmt_bpp(ffi::imgf_decoder_pixfmt(d)) as usize
    };
    let mut hash = FNV_SEED;
    while unsafe { ffi::imgf_decoder_next_row(d, row.as_mut_ptr()) } {
        if keep {
            hash = fnv(hash, &row[..bytes]);
        }
    }
    let err = unsafe { ffi::imgf_decoder_last_error(d) };
    unsafe { ffi::imgf_decoder_destroy(open.d) };
    if err != ffi::IMGF_OK as i32 {
        return Err(format!("decode {err}"));
    }
    Ok(hash)
}

fn decode_rust(format: Format, data: &[u8], options: &DecodeOptions, row: &mut [u8], keep: bool) -> Result<u32, String> {
    let mut d = Decoder::open(format, SliceStream::new(data), options).map_err(|e| format!("open {e}"))?;
    let bytes = d.info().row_bytes();
    let mut hash = FNV_SEED;
    while d.next_row(row).map_err(|e| format!("decode {e}"))? {
        if keep {
            hash = fnv(hash, &row[..bytes]);
        }
    }
    Ok(hash)
}

fn bench_decode(label: &str, format: Format, data: &[u8], tw: u16, th: u16) {
    let options = DecodeOptions { target_width: tw, target_height: th, ..Default::default() };
    let info = match Decoder::open(format, SliceStream::new(data), &options) {
        Ok(d) => d.info(),
        Err(e) => {
            info!(TAG, "{label} FAILED open {e}");
            return;
        }
    };
    let name = format!("{label} decode -> {}x{}", info.width, info.height);
    let mut row = psram(info.row_bytes());
    let c = timed(REPS, |keep| decode_c(format, data, tw, th, &mut row, keep));
    let rust = timed(REPS, |keep| decode_rust(format, data, &options, &mut row, keep));
    report(&name, c, rust, usize::from(info.width) * usize::from(info.height));
}

fn bench_decode_resize(label: &str, data: &[u8], tw: u16, th: u16, format: PixelFormat) {
    let name = format!("{label} decode+resize -> {tw}x{th} {}", format_name(format));
    let bytes = usize::from(tw) * usize::from(th) * format.bytes_per_pixel();
    let c = timed(REPS, |keep| {
        let mut state = unsafe { mem::zeroed() };
        let d = c_open(Format::Jpeg, data, tw, th, &mut state)?;
        let mut o: ffi::imgf_resize_opts_t = unsafe { mem::zeroed() };
        o.target_w = tw;
        o.target_h = th;
        o.dst_pixfmt = c_format(format);
        let (mut out, mut ow, mut oh) = (ptr::null_mut(), 0, 0);
        let err = unsafe { ffi::imgf_resize_decoder(d, &o, &mut out, &mut ow, &mut oh) };
        unsafe { ffi::imgf_decoder_destroy(d) };
        if err != ffi::IMGF_OK as i32 {
            return Err(format!("resize {err}"));
        }
        let hash = if keep { fnv(FNV_SEED, unsafe { slice::from_raw_parts(out, bytes) }) } else { 0 };
        unsafe { ffi::imgf_free(out.cast::<c_void>()) };
        Ok(hash)
    });
    let rust = timed(REPS, |keep| {
        let options = DecodeOptions { target_width: tw, target_height: th, ..Default::default() };
        let mut d = Decoder::open(Format::Jpeg, SliceStream::new(data), &options).map_err(|e| format!("open {e}"))?;
        let o = ResizeOptions { target_width: tw, target_height: th, format: Some(format), ..Default::default() };
        let out = resize_decoder(&mut d, &o, Orientation::None).map_err(|e| format!("resize {e}"))?;
        Ok(if keep { fnv(FNV_SEED, &out.data) } else { 0 })
    });
    report(&name, c, rust, usize::from(tw) * usize::from(th));
}

struct Resize<'a> {
    src: &'a [u8],
    stride: usize,
    sw: u16,
    sh: u16,
    dw: u16,
    dh: u16,
    format: PixelFormat,
}

fn bench_resize(r: &Resize, dst: &mut [u8], place: &str, reps: usize) {
    let name = format!("resize{place} {}x{} -> {}x{} {}", r.sw, r.sh, r.dw, r.dh, format_name(r.format));
    let bytes = usize::from(r.dw) * usize::from(r.dh) * r.format.bytes_per_pixel();
    let c = timed(reps, |keep| {
        let mut o: ffi::imgf_resize_opts_t = unsafe { mem::zeroed() };
        o.target_w = r.dw;
        o.target_h = r.dh;
        o.dst_pixfmt = c_format(r.format);
        let err = unsafe {
            ffi::imgf_resize_buffer(r.src.as_ptr(), r.sw, r.sh, r.stride, ffi::imgf_pixfmt_t_IMGF_PIX_RGB888, dst.as_mut_ptr(), 0, &o)
        };
        if err != ffi::IMGF_OK as i32 {
            return Err(format!("resize {err}"));
        }
        Ok(if keep { fnv(FNV_SEED, &dst[..bytes]) } else { 0 })
    });
    let rust = timed(reps, |keep| {
        let info = ImageInfo { width: r.sw, height: r.sh, format: PixelFormat::Rgb888 };
        let o = ResizeOptions { target_width: r.dw, target_height: r.dh, format: Some(r.format), ..Default::default() };
        resize_buffer(r.src, info, r.stride, dst, 0, &o, Orientation::None).map_err(|e| format!("resize {e}"))?;
        Ok(if keep { fnv(FNV_SEED, &dst[..bytes]) } else { 0 })
    });
    report(&name, c, rust, usize::from(r.dw) * usize::from(r.dh));
}

fn encode_c(src: &[u8], quality: u8, scratch: &mut [u8]) -> Result<usize, String> {
    let mut o: ffi::imgf_jpege_opts_t = unsafe { mem::zeroed() };
    o.quality = i32::from(quality);
    o.subsample = ffi::imgf_jpeg_subsample_t_IMGF_JPEG_SUBSAMPLE_420;
    let mut err = 0;
    let e = unsafe {
        ffi::imgf_jpege_create(SRC_W as u16, SRC_H as u16, ffi::imgf_pixfmt_t_IMGF_PIX_RGB888, &o, &mut err)
    };
    if e.is_null() {
        return Err(format!("create {err}"));
    }
    let mut len = 0;
    let err = unsafe { ffi::imgf_encoder_encode_buffer(e, src.as_ptr(), SRC_W * 3, scratch.as_mut_ptr(), scratch.len(), &mut len) };
    unsafe { ffi::imgf_encoder_destroy(e) };
    if err != ffi::IMGF_OK as i32 {
        return Err(format!("encode {err}"));
    }
    Ok(len)
}

fn encode_rust(src: &[u8], quality: u8, scratch: &mut [u8]) -> Result<usize, String> {
    let options = JpegOptions { quality, subsampling: Subsampling::S420, ..Default::default() };
    let sink = devkit_image::SliceSink::new(scratch);
    let e = JpegEncoder::new(SRC_W as u16, SRC_H as u16, PixelFormat::Rgb888, &options, sink).map_err(|e| format!("create {e}"))?;
    encode_buffer(e, src, SRC_W * 3).map(|(n, _)| n).map_err(|e| format!("encode {e}"))
}

pub fn run() {
    info!(TAG, "devkit_image against image_framework, source {SRC_W}x{SRC_H} RGB888, best of {REPS}");

    let mut src = psram(SRC_W * SRC_H * 3);
    fill_source(&mut src);

    let mut scratch = psram(jpeg_buffer_upper_bound(SRC_W as u16, SRC_H as u16, PixelFormat::Rgb888));
    let mut jpegs = Vec::new();
    for quality in QUALITIES {
        let name = format!("jpeg encode q{quality} 4:2:0");
        let mut len = 0;
        let c = timed(1, |_| encode_c(&src, quality, &mut scratch).map(|n| fnv(FNV_SEED, &scratch[..n])));
        let rust = timed(1, |_| {
            encode_rust(&src, quality, &mut scratch).map(|n| {
                len = n;
                fnv(FNV_SEED, &scratch[..n])
            })
        });
        report(&name, c, rust, SRC_W * SRC_H);
        let mut jpeg = psram(len);
        jpeg.copy_from_slice(&scratch[..len]);
        info!(TAG, "  fixture: q{quality} {len} B");
        jpegs.push(jpeg);
    }
    drop(scratch);

    for (quality, jpeg) in QUALITIES.iter().zip(&jpegs) {
        let label = format!("jpeg q{quality}");
        bench_decode(&label, Format::Jpeg, jpeg, 0, 0);
        bench_decode(&label, Format::Jpeg, jpeg, SRC_W as u16 / 2, SRC_H as u16 / 2);
        bench_decode(&label, Format::Jpeg, jpeg, SRC_W as u16 / 4, SRC_H as u16 / 4);
        bench_decode_resize(&label, jpeg, 1280, 720, PixelFormat::Rgb565);
        bench_decode_resize(&label, jpeg, 1280, 720, PixelFormat::Rgb888);
        bench_decode_resize(&label, jpeg, 1280, 720, PixelFormat::Bgr888);
        bench_decode_resize(&label, jpeg, 640, 360, PixelFormat::Rgb565);
    }

    bench_decode("png", Format::Png, PNG, 0, 0);
    for (label, format, data) in [
        ("jpeg progressive q85", Format::Jpeg, PROGRESSIVE),
        ("webp lossy", Format::Webp, WEBP_LOSSY),
        ("webp lossless", Format::Webp, WEBP_LOSSLESS),
    ] {
        if data.is_empty() {
            info!(TAG, "{label}: no asset, skipped");
        } else {
            bench_decode(label, format, data, 0, 0);
        }
    }

    let mut dst = psram(SRC_W * SRC_H * 3);
    let stride = SRC_W * 3;
    for (sw, sh, dw, dh, format) in [
        (1920, 1080, 1280, 720, PixelFormat::Rgb888),
        (1920, 1080, 1280, 720, PixelFormat::Rgb565),
        (1920, 1080, 960, 540, PixelFormat::Rgb888),
        (1920, 1080, 640, 360, PixelFormat::Rgb888),
        (640, 360, 1280, 720, PixelFormat::Rgb888),
    ] {
        bench_resize(&Resize { src: &src, stride, sw, sh, dw, dh, format }, &mut dst, "", REPS);
    }

    const INT_SW: usize = 256;
    const INT_SH: usize = 144;
    const INT_DW: usize = 171;
    const INT_DH: usize = 96;
    let mut small_src = block(INT_SW * INT_SH * 3, internal_caps());
    let mut small_dst = block(INT_DW * INT_DH * 3, internal_caps());
    for (y, row) in small_src.chunks_exact_mut(INT_SW * 3).enumerate() {
        row.copy_from_slice(&src[y * stride..y * stride + INT_SW * 3]);
    }
    let r = Resize {
        src: &small_src,
        stride: INT_SW * 3,
        sw: INT_SW as u16,
        sh: INT_SH as u16,
        dw: INT_DW as u16,
        dh: INT_DH as u16,
        format: PixelFormat::Rgb888,
    };
    bench_resize(&r, &mut small_dst, " (internal)", 30);

    info!(TAG, "bench done");
}
