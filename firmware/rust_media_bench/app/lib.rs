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

use alloc::vec::Vec;
use core::ffi::CStr;
use core::mem;
use core::ptr;
use core::slice;
use core::time::Duration;

use devkit_media::{Arena, Buffer, Demuxer, OpenOptions};
use idf::heap::{self, Caps};
use idf::log::{error, info};
use idf::thread;
use idf::time::Instant;

const TAG: &CStr = c"bench";
const ROUNDS: usize = 7;
const ARENA_BYTES: usize = 4 * 1024 * 1024;
const STREAM_BYTES: usize = 16 * 1024 * 1024;
const SEEKS: i64 = 16;

const AVI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.avi"));
const WAV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.wav"));
const MP3: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.mp3"));
const FLAC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.flac"));
const OPUS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.opus"));
const MKV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.mkv"));
const MP4: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bench.mp4"));

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    packets: u32,
    bytes: u64,
    sum: u32,
}

impl Tally {
    #[inline]
    fn add(&mut self, data: &[u8]) {
        self.packets += 1;
        self.bytes += data.len() as u64;
        self.sum = self.sum.wrapping_mul(31).wrapping_add(u32::from(data[0]) + u32::from(data[data.len() - 1]));
    }
}

struct File {
    name: &'static str,
    path: &'static CStr,
}

fn add_file(name: &'static CStr, data: &[u8]) -> File {
    let mut copy = heap::try_vec(data.len(), Caps::SPIRAM).expect("bench file");
    copy.extend_from_slice(data);
    serve(name, Vec::leak(copy))
}

fn serve(name: &'static CStr, data: &'static [u8]) -> File {
    let path = unsafe { ffi::bench_file_add(name.as_ptr(), data.as_ptr().cast(), data.len()) };
    assert!(!path.is_null());
    File { name: name.to_str().unwrap_or("?"), path: unsafe { CStr::from_ptr(path) } }
}

fn arena_memory() -> &'static mut [u8] {
    #[repr(C, align(64))]
    struct Line([u8; 64]);
    unsafe impl devkit::Zeroable for Line {}
    let lines = heap::zeroed_slice::<Line>(ARENA_BYTES / 64, Caps::SPIRAM).expect("arena");
    let lines: &'static mut [Line] = alloc::boxed::Box::leak(lines);
    unsafe { slice::from_raw_parts_mut(lines.as_mut_ptr().cast(), ARENA_BYTES) }
}

// The arena is handed out again for every run; nothing outlives one.
fn reuse(memory: &mut [u8]) -> &'static mut [u8] {
    unsafe { slice::from_raw_parts_mut(memory.as_mut_ptr(), memory.len()) }
}

fn run_rust(file: &File, memory: &mut [u8]) -> Tally {
    let options = OpenOptions { want_cover: true };
    let mut demux = devkit_media::open(file.path, Arena::new(reuse(memory)), &options).expect("open");
    let mut tally = Tally::default();
    while let Ok(Some(packet)) = demux.read() {
        tally.add(&packet.data);
    }
    let duration = demux.info().duration_us;
    for k in 0..SEEKS {
        if let Ok(landed) = demux.seek(duration * k / SEEKS) {
            tally.sum = tally.sum.wrapping_add(landed as u32);
            if let Ok(Some(packet)) = demux.read() {
                tally.add(&packet.data);
            }
        }
    }
    tally
}

fn arena_c(memory: &mut [u8]) -> ffi::medf_arena_t {
    ffi::medf_arena_t { data: memory.as_mut_ptr(), size: memory.len(), direct: false }
}

macro_rules! c_video {
    ($name:ident, $open:ident, $close:ident, $info:ident, $read:ident, $buffer:ident, $seek:ident, $packet:ident) => {
        fn $name(file: &File, memory: &mut [u8]) -> Tally {
            unsafe {
                let arena = arena_c(memory);
                let demux = ffi::$open(file.path.as_ptr(), &arena, true, ptr::null_mut());
                assert!(!demux.is_null());
                let mut tally = Tally::default();
                let mut packet: ffi::$packet = mem::zeroed();
                while ffi::$read(demux, &mut packet, true) {
                    tally.add(slice::from_raw_parts(packet.data, packet.size as usize));
                    ffi::medf_buffer_release(ffi::$buffer(demux), packet.ref_);
                }
                let duration = (*ffi::$info(demux)).duration_us;
                for k in 0..SEEKS {
                    let mut landed = 0;
                    if ffi::$seek(demux, duration * k / SEEKS, &mut landed) {
                        tally.sum = tally.sum.wrapping_add(landed as u32);
                        if ffi::$read(demux, &mut packet, true) {
                            tally.add(slice::from_raw_parts(packet.data, packet.size as usize));
                            ffi::medf_buffer_release(ffi::$buffer(demux), packet.ref_);
                        }
                    }
                }
                ffi::$close(demux);
                tally
            }
        }
    };
}

c_video!(c_mkv, medf_mkv_demux_open, medf_mkv_demux_close, medf_mkv_demux_info, medf_mkv_demux_read, medf_mkv_demux_buffer,
         medf_mkv_demux_seek, medf_mkv_packet_t);
c_video!(c_mp4, medf_mp4_demux_open, medf_mp4_demux_close, medf_mp4_demux_info, medf_mp4_demux_read, medf_mp4_demux_buffer,
         medf_mp4_demux_seek, medf_mp4_packet_t);

fn c_avi(file: &File, memory: &mut [u8]) -> Tally {
    unsafe {
        let arena = arena_c(memory);
        let demux = ffi::medf_avi_demux_open(file.path.as_ptr(), &arena, true, ptr::null_mut());
        assert!(!demux.is_null());
        let mut tally = Tally::default();
        let mut packet: ffi::medf_avi_packet_t = mem::zeroed();
        while ffi::medf_avi_demux_read(demux, &mut packet, true) {
            tally.add(slice::from_raw_parts(packet.data, packet.size as usize));
            ffi::medf_buffer_release(ffi::medf_avi_demux_buffer(demux), packet.ref_);
        }
        let info = &*ffi::medf_avi_demux_info(demux);
        let interval = info.video.frame_interval_us;
        let duration = i64::from(info.video.frame_count) * i64::from(interval);
        for k in 0..SEEKS {
            let mut landed = 0;
            let frame = (duration * k / SEEKS / i64::from(interval)) as u32;
            if ffi::medf_avi_demux_seek(demux, frame, &mut landed) {
                tally.sum = tally.sum.wrapping_add(landed.wrapping_mul(interval));
                if ffi::medf_avi_demux_read(demux, &mut packet, true) {
                    tally.add(slice::from_raw_parts(packet.data, packet.size as usize));
                    ffi::medf_buffer_release(ffi::medf_avi_demux_buffer(demux), packet.ref_);
                }
            }
        }
        ffi::medf_avi_demux_close(demux);
        tally
    }
}

macro_rules! c_audio {
    ($name:ident, $open:ident, $close:ident, $info:ident, $read:ident, $seek:ident, $packet:ident, $release:expr) => {
        fn $name(file: &File, memory: &mut [u8]) -> Tally {
            unsafe {
                let arena = arena_c(memory);
                let demux = ffi::$open(file.path.as_ptr(), &arena, true, ptr::null_mut());
                assert!(!demux.is_null());
                let mut tally = Tally::default();
                let mut packet: ffi::$packet = mem::zeroed();
                while ffi::$read(demux, &mut packet) {
                    tally.add(slice::from_raw_parts(packet.data, packet.size as usize));
                    $release(demux, packet.ref_);
                }
                let duration = (*ffi::$info(demux)).duration_us;
                for k in 0..SEEKS {
                    let mut landed = 0;
                    if ffi::$seek(demux, duration * k / SEEKS, &mut landed) {
                        tally.sum = tally.sum.wrapping_add(landed as u32);
                        if ffi::$read(demux, &mut packet) {
                            tally.add(slice::from_raw_parts(packet.data, packet.size as usize));
                            $release(demux, packet.ref_);
                        }
                    }
                }
                ffi::$close(demux);
                tally
            }
        }
    };
}

c_audio!(c_wav, medf_wav_demux_open, medf_wav_demux_close, medf_wav_demux_info, medf_wav_demux_read, medf_wav_demux_seek, medf_wav_packet_t,
         |d, r| ffi::medf_buffer_release(ffi::medf_wav_demux_buffer(d), r));
c_audio!(c_es, medf_es_demux_open, medf_es_demux_close, medf_es_demux_info, medf_es_demux_read, medf_es_demux_seek, medf_es_packet_t,
         |d, r| ffi::medf_buffer_release(ffi::medf_es_demux_buffer(d), r));
c_audio!(c_flac, medf_flac_demux_open, medf_flac_demux_close, medf_flac_demux_info, medf_flac_demux_read, medf_flac_demux_seek, medf_flac_packet_t,
         |d, r| ffi::medf_buffer_release(ffi::medf_flac_demux_buffer(d), r));
c_audio!(c_ogg, medf_ogg_demux_open, medf_ogg_demux_close, medf_ogg_demux_info, medf_ogg_demux_read, medf_ogg_demux_seek, medf_ogg_packet_t,
         |d, r| ffi::medf_ogg_demux_release(d, r));

struct Best {
    time: Duration,
    tally: Tally,
}

impl Best {
    const fn new() -> Self {
        Self { time: Duration::MAX, tally: Tally { packets: 0, bytes: 0, sum: 0 } }
    }

    fn run(&mut self, f: impl FnOnce() -> Tally) {
        let start = Instant::now();
        let tally = f();
        self.time = self.time.min(start.elapsed());
        self.tally = tally;
    }
}

fn report(name: &str, c: &Best, rust: &Best) {
    let (c_us, rust_us) = (c.time.as_micros() as u32, rust.time.as_micros() as u32);
    info!(TAG, "{name}: {} packets {} bytes, C {c_us} us, Rust {rust_us} us, ratio {:.3}", rust.tally.packets, rust.tally.bytes,
          rust_us as f32 / c_us as f32);
    if c.tally != rust.tally {
        error!(TAG, "{name}: C saw {} packets {} bytes sum {:08x}, Rust {} packets {} bytes sum {:08x}", c.tally.packets, c.tally.bytes,
               c.tally.sum, rust.tally.packets, rust.tally.bytes, rust.tally.sum);
    }
}

fn stream_rust(file: &File, memory: &mut [u8], chunk: usize) -> Tally {
    let buffer = Buffer::open(file.path, Arena::new(reuse(memory))).expect("open");
    buffer.set_readahead(true);
    let mut tally = Tally::default();
    while let Ok(view) = buffer.view(chunk) {
        tally.add(&view);
    }
    tally
}

fn stream_c(file: &File, memory: &mut [u8], chunk: usize) -> Tally {
    unsafe {
        let arena = arena_c(memory);
        let buffer = ffi::medf_buffer_open(file.path.as_ptr(), &arena);
        assert!(!buffer.is_null());
        ffi::medf_buffer_set_readahead(buffer, true);
        let mut tally = Tally::default();
        let mut reference = 0;
        loop {
            let data = ffi::medf_buffer_view(buffer, chunk, &mut reference);
            if data.is_null() {
                break;
            }
            tally.add(slice::from_raw_parts(data, chunk));
            ffi::medf_buffer_release(buffer, reference);
        }
        ffi::medf_buffer_close(buffer);
        tally
    }
}

pub fn run() {
    let files = [
        (add_file(c"bench.avi", AVI), c_avi as fn(&File, &mut [u8]) -> Tally),
        (add_file(c"bench.wav", WAV), c_wav),
        (add_file(c"bench.mp3", MP3), c_es),
        (add_file(c"bench.flac", FLAC), c_flac),
        (add_file(c"bench.opus", OPUS), c_ogg),
        (add_file(c"bench.mkv", MKV), c_mkv),
        (add_file(c"bench.mp4", MP4), c_mp4),
    ];
    let memory = arena_memory();

    let mut pattern: Vec<u8, Caps> = heap::try_vec(STREAM_BYTES, Caps::SPIRAM).expect("stream");
    pattern.resize(STREAM_BYTES, 0x5A);
    let stream = serve(c"bench.bin", Vec::leak(pattern));

    for (file, c) in &files {
        let (mut c_best, mut rust_best) = (Best::new(), Best::new());
        for _ in 0..ROUNDS {
            c_best.run(|| c(file, memory));
            rust_best.run(|| run_rust(file, memory));
        }
        report(file.name, &c_best, &rust_best);
    }

    for chunk in [1024, 32 * 1024] {
        let (mut c_best, mut rust_best) = (Best::new(), Best::new());
        for _ in 0..ROUNDS {
            c_best.run(|| stream_c(&stream, memory, chunk));
            rust_best.run(|| stream_rust(&stream, memory, chunk));
        }
        let mbps = |best: &Best| best.tally.bytes as f32 / best.time.as_micros() as f32;
        report(if chunk == 1024 { "views 1 KB" } else { "views 32 KB" }, &c_best, &rust_best);
        info!(TAG, "throughput C {:.1} MB/s, Rust {:.1} MB/s", mbps(&c_best), mbps(&rust_best));
    }

    info!(TAG, "stack left {} bytes", thread::current().stack_high_water_mark());
    info!(TAG, "done");
}
