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

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{CStr, c_void};
use core::ptr;
use core::time::Duration;

use devkit::Zeroable;
use devkit_compression::{Inflate, Lzw, LzwOptions, LzwWork, Window};
use idf::heap::{self, Caps};
use idf::log::{error, info};
use idf::time::Instant;

const TAG: &CStr = c"bench";
const ROUNDS: usize = 7;
const CHUNK: usize = 4096;
const RAW_BYTES: usize = 1 << 20;

const RAW: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/raw.bin"));
const ZLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/zlib.bin"));
const GIF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/gif.bin"));
const TIFF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tiff.bin"));

const GIF_OPTIONS: LzwOptions = LzwOptions { literal_bits: 8, msb_first: false, early_change: false };
const TIFF_OPTIONS: LzwOptions = LzwOptions { literal_bits: 8, msb_first: true, early_change: true };

fn fnv(hash: u32, data: &[u8]) -> u32 {
    data.iter().fold(hash, |h, &b| (h ^ u32::from(b)).wrapping_mul(16777619))
}

const FNV_SEED: u32 = 2166136261;

fn in_psram(data: &[u8]) -> Vec<u8, Caps> {
    let mut v = heap::try_vec(data.len(), Caps::SPIRAM).expect("bench input");
    v.extend_from_slice(data);
    v
}

fn zeroed<T: Zeroable>() -> Box<T, Caps> {
    heap::zeroed_box(Caps::ACCESS_8BIT | Caps::INTERNAL).expect("bench state")
}

// Bindgen structs are plain C data.
struct Plain<T>(T);
unsafe impl<T> Zeroable for Plain<T> {}

struct Timed {
    best: Duration,
    hash: u32,
}

impl Timed {
    fn new() -> Self {
        Self { best: Duration::MAX, hash: 0 }
    }

    fn run(&mut self, f: impl FnOnce(&mut dyn FnMut(&[u8])) -> usize) {
        let mut hash = FNV_SEED;
        // Hashing every byte would drown the decoder.
        let mut consume = |chunk: &[u8]| hash = fnv(hash, &chunk[chunk.len().saturating_sub(64)..]);
        let start = Instant::now();
        let total = f(&mut consume);
        let elapsed = start.elapsed();
        assert_eq!(total, RAW_BYTES);
        self.best = self.best.min(elapsed);
        self.hash = hash;
    }
}

fn inflate_rust(input: &[u8], z: &mut Inflate, window: &mut Window, out: &mut [u8], sink: &mut dyn FnMut(&[u8])) -> usize {
    let mut src = input;
    z.start(&mut src).expect("zlib header");
    let mut total = 0;
    loop {
        let n = z.read(&mut src, window, out).expect("inflate");
        sink(&out[..n]);
        total += n;
        if n < out.len() {
            return total;
        }
    }
}

fn lzw_rust(input: &[u8], options: &LzwOptions, z: &mut Lzw, work: &mut LzwWork, out: &mut [u8], sink: &mut dyn FnMut(&[u8])) -> usize {
    let mut src = input;
    z.start(options, work).expect("lzw options");
    let mut total = 0;
    loop {
        let n = z.read(&mut src, work, out).expect("lzw");
        sink(&out[..n]);
        total += n;
        if n < out.len() {
            return total;
        }
    }
}

struct Block {
    data: *const u8,
    len: usize,
}

unsafe extern "C" fn refill(user: *mut c_void, n: *mut usize) -> *const u8 {
    let block = unsafe { &mut *user.cast::<Block>() };
    unsafe { *n = block.len };
    block.len = 0;
    block.data
}

fn inflate_c(input: &[u8], z: &mut ffi::cmpf_inflate_t, window: &mut Window, out: &mut [u8], sink: &mut dyn FnMut(&[u8])) -> usize {
    let mut block = Block { data: input.as_ptr(), len: input.len() };
    let src = ffi::cmpf_src_t { refill: Some(refill), user: ptr::from_mut(&mut block).cast() };
    assert!(unsafe { ffi::cmpf_inflate_init(z, src, window.as_mut_ptr()) });
    let mut total = 0;
    loop {
        let n = unsafe { ffi::cmpf_inflate_read(z, out.as_mut_ptr(), out.len()) };
        assert_eq!(z.err, 0);
        sink(&out[..n]);
        total += n;
        if n < out.len() {
            return total;
        }
    }
}

fn lzw_c(input: &[u8], options: &LzwOptions, z: &mut ffi::cmpf_lzw_t, work: &mut LzwWork, out: &mut [u8], sink: &mut dyn FnMut(&[u8])) -> usize {
    let mut block = Block { data: input.as_ptr(), len: input.len() };
    let src = ffi::cmpf_src_t { refill: Some(refill), user: ptr::from_mut(&mut block).cast() };
    let opts = ffi::cmpf_lzw_opts_t {
        literal_bits: options.literal_bits,
        msb_first: options.msb_first.into(),
        early_change: options.early_change.into(),
    };
    assert!(unsafe { ffi::cmpf_lzw_init(z, src, &opts, ptr::from_mut(work).cast()) });
    let mut total = 0;
    loop {
        let n = unsafe { ffi::cmpf_lzw_read(z, out.as_mut_ptr(), out.len()) };
        assert_eq!(z.err, 0);
        sink(&out[..n]);
        total += n;
        if n < out.len() {
            return total;
        }
    }
}

fn report(name: &str, c: &Timed, rust: &Timed, input_bytes: usize, expected: u32) {
    let (c_us, rust_us) = (c.best.as_micros() as u32, rust.best.as_micros() as u32);
    info!(TAG, "{name}: {input_bytes} -> {RAW_BYTES} bytes, C {c_us} us, Rust {rust_us} us, ratio {:.3}",
          rust_us as f32 / c_us as f32);
    if c.hash != expected || rust.hash != expected {
        error!(TAG, "{name}: checksum C {:08x} Rust {:08x} expected {expected:08x}", c.hash, rust.hash);
    }
}

fn expected_hash(out_chunk: usize) -> u32 {
    let mut hash = FNV_SEED;
    for chunk in RAW.chunks(out_chunk) {
        hash = fnv(hash, &chunk[chunk.len().saturating_sub(64)..]);
    }
    hash
}

pub fn run() {
    let zlib = in_psram(ZLIB);
    let gif = in_psram(GIF);
    let tiff = in_psram(TIFF);
    let mut out: Vec<u8, Caps> = heap::try_vec(CHUNK, Caps::ACCESS_8BIT | Caps::INTERNAL).expect("out");
    out.resize(CHUNK, 0);
    let mut window = zeroed::<Window>();
    let mut work = zeroed::<LzwWork>();
    let mut inflate = zeroed::<Inflate>();
    let mut lzw = zeroed::<Lzw>();
    let mut c_inflate = zeroed::<Plain<ffi::cmpf_inflate_t>>();
    let mut c_lzw = zeroed::<Plain<ffi::cmpf_lzw_t>>();
    let expected = expected_hash(CHUNK);

    let mut cases = [(Timed::new(), Timed::new()), (Timed::new(), Timed::new()), (Timed::new(), Timed::new())];
    for _ in 0..ROUNDS {
        let [zlib_case, gif_case, tiff_case] = &mut cases;
        zlib_case.0.run(|sink| inflate_c(&zlib, &mut c_inflate.0, &mut window, &mut out, sink));
        zlib_case.1.run(|sink| inflate_rust(&zlib, &mut inflate, &mut window, &mut out, sink));
        gif_case.0.run(|sink| lzw_c(&gif, &GIF_OPTIONS, &mut c_lzw.0, &mut work, &mut out, sink));
        gif_case.1.run(|sink| lzw_rust(&gif, &GIF_OPTIONS, &mut lzw, &mut work, &mut out, sink));
        tiff_case.0.run(|sink| lzw_c(&tiff, &TIFF_OPTIONS, &mut c_lzw.0, &mut work, &mut out, sink));
        tiff_case.1.run(|sink| lzw_rust(&tiff, &TIFF_OPTIONS, &mut lzw, &mut work, &mut out, sink));
    }
    let [zlib_case, gif_case, tiff_case] = &cases;
    report("inflate", &zlib_case.0, &zlib_case.1, zlib.len(), expected);
    report("lzw gif", &gif_case.0, &gif_case.1, gif.len(), expected);
    report("lzw tiff", &tiff_case.0, &tiff_case.1, tiff.len(), expected);
    info!(TAG, "BENCH DONE");
    #[cfg(not(target_os = "none"))]
    idf::system::restart();
}
