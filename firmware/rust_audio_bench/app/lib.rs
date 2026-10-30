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
use core::ffi::CStr;
use core::ptr;
use core::time::Duration;

use devkit_audio::aac::{self, AacConfig, AacDecoder, HeMode};
use devkit_audio::alac::{AlacConfig, AlacDecoder};
use devkit_audio::eq::{Biquad, Eq, EqConfig};
use devkit_audio::flac::{FlacConfig, FlacDecoder};
use devkit_audio::gain::{Gain, GainConfig};
use devkit_audio::mp3::{self, Mp3Config, Mp3Decoder};
use devkit_audio::opus::{OpusConfig, OpusDecoder};
use devkit_audio::{Decoder, Format, PcmMut};
use idf::heap::{self, Caps};
use idf::log::{error, info};
use idf::thread;
use idf::time::Instant;

const TAG: &CStr = c"bench";
const ROUNDS: usize = 3;
const FNV_SEED: u32 = 2166136261;

macro_rules! clip {
    ($name:literal) => {
        ($name, include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".pkt")).as_slice())
    };
}

const CLIPS: [(&str, &[u8]); 12] = [
    clip!("mp3_l3_44_128mp3_2"),
    clip!("mp3_l3_44_320mp3_2"),
    clip!("mp3_l2_44_256mp2_2"),
    clip!("aac_adts_lc_44_2_128_he0"),
    clip!("aac_adts_he_48_2_64_he1"),
    clip!("aac_adts_ps_48_2_32_he2"),
    clip!("opus_music_128k"),
    clip!("opus_speech_16k_stereo"),
    clip!("opus_speech_8k"),
    clip!("flac_s16_44_l5"),
    clip!("flac_s24_96_l5"),
    clip!("alac_ff_s16_44"),
];

fn u32le(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

struct Stream {
    data: Vec<u8, Caps>,
    codec: u8,
    p1: u8,
    p2: u8,
    config: (usize, usize),
    packets: Vec<(usize, usize)>,
}

fn psram() -> Caps {
    Caps::SPIRAM | Caps::ACCESS_8BIT
}

impl Stream {
    fn new(clip: &[u8]) -> Self {
        let mut data = heap::try_vec(clip.len(), psram()).expect("clip");
        data.extend_from_slice(clip);
        let cfg_len = u32le(&clip[8..]) as usize;
        let mut at = 12 + cfg_len;
        let count = u32le(&clip[at..]) as usize;
        at += 4;
        let mut packets = Vec::with_capacity(count);
        for _ in 0..count {
            let len = u32le(&clip[at..]) as usize;
            packets.push((at + 4, len));
            at += 4 + len;
        }
        Self { data, codec: clip[4], p1: clip[5], p2: clip[6], config: (12, cfg_len), packets }
    }

    fn config(&self) -> &[u8] {
        &self.data[self.config.0..self.config.0 + self.config.1]
    }

    fn packets(&self) -> impl Iterator<Item = &[u8]> {
        self.packets.iter().map(|&(at, len)| &self.data[at..at + len])
    }

    fn rust(&self) -> Box<dyn Decoder> {
        let caps = psram();
        let config = self.config();
        match self.codec {
            0 => {
                let he = [HeMode::Off, HeMode::V1, HeMode::V2][usize::from(self.p2)];
                Box::new(AacDecoder::new(&AacConfig { asc: config, adts: self.p1 != 0, he, caps, scratch_caps: None, clock: None }).expect("aac"))
            }
            1 => Box::new(Mp3Decoder::new(&Mp3Config { channels: self.p1, caps }).expect("mp3")),
            2 => Box::new(OpusDecoder::new(&OpusConfig { head: config, caps }).expect("opus")),
            3 => Box::new(FlacDecoder::new(&FlacConfig { streaminfo: config, caps }).expect("flac")),
            _ => Box::new(AlacDecoder::new(&AlacConfig { cookie: config, caps }).expect("alac")),
        }
    }

    fn c(&self) -> CDecoder {
        let caps = psram().bits();
        let config = self.config();
        let mut dec = ptr::null_mut();
        let err = unsafe {
            match self.codec {
                0 => {
                    let mut c: ffi::audf_aac_config_t = core::mem::zeroed();
                    c.asc = config.as_ptr();
                    c.asc_len = config.len();
                    c.adts = self.p1 != 0;
                    c.he = u32::from(self.p2) as _;
                    c.alloc_caps = caps;
                    ffi::audf_aac_decoder_create(&c, &mut dec)
                }
                1 => ffi::audf_mp3_decoder_create(&ffi::audf_mp3_config_t { channels: self.p1, alloc_caps: caps }, &mut dec),
                2 => ffi::audf_opus_decoder_create(&ffi::audf_opus_config_t { head: config.as_ptr(), head_len: config.len(), alloc_caps: caps }, &mut dec),
                3 => ffi::audf_flac_decoder_create(&ffi::audf_flac_config_t { streaminfo: config.as_ptr(), streaminfo_len: config.len(), alloc_caps: caps }, &mut dec),
                _ => ffi::audf_alac_decoder_create(&ffi::audf_alac_config_t { cookie: config.as_ptr(), cookie_len: config.len(), alloc_caps: caps }, &mut dec),
            }
        };
        assert_eq!(err, 0);
        CDecoder(dec)
    }

    fn rate(&self) -> u32 {
        let dec = self.c();
        unsafe {
            match self.codec {
                0 => ffi::audf_aac_decoder_rate(dec.0),
                1 => mp3::parse_header(&self.data[self.packets[0].0..]).map_or(0, |h| h.rate),
                2 => 48000,
                3 => ffi::audf_flac_decoder_rate(dec.0),
                _ => ffi::audf_alac_decoder_rate(dec.0),
            }
        }
    }
}

struct CDecoder(*mut ffi::audf_decoder_t);

impl Drop for CDecoder {
    fn drop(&mut self) {
        unsafe { ffi::audf_decoder_destroy(self.0) };
    }
}

struct Pcm {
    s16: Vec<i16, Caps>,
    s32: Vec<i32, Caps>,
}

impl Pcm {
    fn new(samples: usize) -> Self {
        let mut s16 = heap::try_vec(samples, Caps::SIMD).expect("pcm");
        s16.resize(samples, 0);
        let mut s32 = heap::try_vec(samples, Caps::SIMD).expect("pcm");
        s32.resize(samples, 0);
        Self { s16, s32 }
    }
}

fn fnv(hash: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(hash, |h, &b| (h ^ u32::from(b)).wrapping_mul(16777619))
}

fn hash_pcm(hash: u32, format: Format, pcm: &Pcm, samples: usize) -> u32 {
    let bytes = match format {
        Format::S16 => unsafe { core::slice::from_raw_parts(pcm.s16.as_ptr().cast::<u8>(), samples * 2) },
        Format::S32 => unsafe { core::slice::from_raw_parts(pcm.s32.as_ptr().cast::<u8>(), samples * 4) },
    };
    fnv(hash, bytes)
}

struct Run {
    best: Duration,
    frames: usize,
    hash: u32,
}

fn run_rust(s: &Stream, pcm: &mut Pcm) -> Run {
    let mut dec = s.rust();
    let (format, channels) = (dec.format(), usize::from(dec.channels()));
    let mut best = Duration::MAX;
    let mut frames = 0;
    for _ in 0..ROUNDS {
        dec.reset();
        frames = 0;
        let start = Instant::now();
        for packet in s.packets() {
            let out = match format {
                Format::S16 => PcmMut::S16(&mut pcm.s16),
                Format::S32 => PcmMut::S32(&mut pcm.s32),
            };
            frames += dec.decode(packet, out).expect("rust decode");
        }
        best = best.min(start.elapsed());
    }
    dec.reset();
    let mut hash = FNV_SEED;
    for packet in s.packets() {
        let out = match format {
            Format::S16 => PcmMut::S16(&mut pcm.s16),
            Format::S32 => PcmMut::S32(&mut pcm.s32),
        };
        let n = dec.decode(packet, out).expect("rust decode");
        hash = hash_pcm(hash, format, pcm, n * channels);
    }
    Run { best, frames, hash }
}

fn run_c(s: &Stream, pcm: &mut Pcm) -> Run {
    let dec = s.c();
    let format = if unsafe { ffi::audf_decoder_fmt(dec.0) } == ffi::audf_fmt_t_AUDF_FMT_S16 { Format::S16 } else { Format::S32 };
    let channels = usize::from(unsafe { ffi::audf_decoder_channels(dec.0) });
    let out = match format {
        Format::S16 => pcm.s16.as_mut_ptr().cast(),
        Format::S32 => pcm.s32.as_mut_ptr().cast(),
    };
    let mut best = Duration::MAX;
    let mut frames = 0;
    for _ in 0..ROUNDS {
        unsafe { ffi::audf_decoder_reset(dec.0) };
        frames = 0;
        let start = Instant::now();
        for packet in s.packets() {
            let mut n = 0;
            assert_eq!(unsafe { ffi::audf_decoder_decode(dec.0, packet.as_ptr().cast(), packet.len(), out, &mut n) }, 0);
            frames += n;
        }
        best = best.min(start.elapsed());
    }
    unsafe { ffi::audf_decoder_reset(dec.0) };
    let mut hash = FNV_SEED;
    for packet in s.packets() {
        let mut n = 0;
        assert_eq!(unsafe { ffi::audf_decoder_decode(dec.0, packet.as_ptr().cast(), packet.len(), out, &mut n) }, 0);
        hash = hash_pcm(hash, format, pcm, n * channels);
    }
    Run { best, frames, hash }
}

fn percent(run: &Run, rate: u32) -> f32 {
    run.best.as_secs_f32() * rate as f32 / run.frames as f32 * 100.0
}

fn bench_clip(name: &str, clip: &[u8], pcm: &mut Pcm) {
    let s = Stream::new(clip);
    let rate = s.rate();
    let r = run_rust(&s, pcm);
    let c = run_c(&s, pcm);
    let (cp, rp) = (percent(&c, rate), percent(&r, rate));
    info!(TAG, "{name}: {} frames at {rate} Hz, C {cp:.2} %, Rust {rp:.2} %, ratio {:.3}, hash {}", c.frames, rp / cp, if c.hash == r.hash { "match" } else { "DIFFERS" });
    if c.hash != r.hash || c.frames != r.frames {
        error!(TAG, "{name}: C {:08x} {} frames, Rust {:08x} {} frames", c.hash, c.frames, r.hash, r.frames);
    }
}

fn bench_dsp(pcm: &mut Pcm) {
    const FRAMES: usize = 512;
    const BLOCKS: usize = 94;
    let rate = 48000;
    let biquads = [
        Biquad::low_shelf(rate, 100.0, 0.7, 3.0),
        Biquad::peaking(rate, 500.0, 1.0, -2.0),
        Biquad::peaking(rate, 2000.0, 1.0, 2.0),
        Biquad::peaking(rate, 6000.0, 1.0, 1.0),
        Biquad::high_shelf(rate, 10000.0, 0.7, 2.0),
    ];
    let source: Vec<i32> = (0..FRAMES * 2).map(|i| (i as i32 * 7919) % (1 << 22) - (1 << 21)).collect();

    let (mut eq, control) = Eq::<i32>::new(&EqConfig { channels: 2, enabled: true, caps: psram(), ..Default::default() }).expect("eq");
    control.set_biquads(&biquads).expect("biquads");
    let (mut gain, _) = Gain::<i32>::new(&GainConfig { channels: 2, sample_rate: rate, gain: 0.7, caps: psram() }).expect("gain");
    let start = Instant::now();
    let mut rust_hash = FNV_SEED;
    let buf = &mut pcm.s32[..FRAMES * 2];
    for _ in 0..BLOCKS {
        buf.copy_from_slice(&source);
        eq.process_in_place(buf);
        gain.process_in_place(buf);
    }
    let rust = start.elapsed();
    rust_hash = fnv(rust_hash, unsafe { core::slice::from_raw_parts(buf.as_ptr().cast(), buf.len() * 4) });

    let mut c_eq = ptr::null_mut();
    let mut c_gain = ptr::null_mut();
    let caps = psram().bits();
    unsafe {
        let mut config: ffi::audf_eq_config_t = core::mem::zeroed();
        config.fmt = ffi::audf_fmt_t_AUDF_FMT_S32;
        config.channels = 2;
        config.enabled = true;
        config.alloc_caps = caps;
        assert_eq!(ffi::audf_eq_create(&config, &mut c_eq), 0);
        let c_biquads: Vec<ffi::audf_biquad_t> = biquads.iter().map(|b| ffi::audf_biquad_t { b0: b.b0, b1: b.b1, b2: b.b2, a1: b.a1, a2: b.a2 }).collect();
        assert_eq!(ffi::audf_eq_set_biquads(c_eq, c_biquads.as_ptr(), c_biquads.len()), 0);
        let mut config: ffi::audf_gain_config_t = core::mem::zeroed();
        config.fmt = ffi::audf_fmt_t_AUDF_FMT_S32;
        config.channels = 2;
        config.sample_rate = rate;
        config.gain = 0.7;
        config.alloc_caps = caps;
        assert_eq!(ffi::audf_gain_create(&config, &mut c_gain), 0);
    }
    let p = buf.as_mut_ptr().cast();
    let start = Instant::now();
    for _ in 0..BLOCKS {
        buf.copy_from_slice(&source);
        unsafe {
            ffi::audf_eq_process(c_eq, p, p, FRAMES);
            ffi::audf_gain_process(c_gain, p, p, FRAMES);
        }
    }
    let c = start.elapsed();
    let c_hash = fnv(FNV_SEED, unsafe { core::slice::from_raw_parts(buf.as_ptr().cast(), buf.len() * 4) });
    unsafe {
        ffi::audf_eq_destroy(c_eq);
        ffi::audf_gain_destroy(c_gain);
    }
    let (cp, rp) = (c.as_secs_f32() * 100.0, rust.as_secs_f32() * 100.0);
    info!(TAG, "dsp eq5+gain s32 stereo: C {cp:.2} %, Rust {rp:.2} %, ratio {:.3}, hash {}", rp / cp, if c_hash == rust_hash { "match" } else { "DIFFERS" });
}

extern "C" fn c_clock() -> u32 {
    Instant::now().since_boot().as_micros() as u32
}

fn rust_clock() -> u32 {
    c_clock()
}

fn aac_profile(name: &str, clip: &[u8], pcm: &mut Pcm) {
    let s = Stream::new(clip);
    let mut c_prof = [0u64; 6];
    let mut r_prof = [0u64; 6];
    let he = [HeMode::Off, HeMode::V1, HeMode::V2][usize::from(s.p2)];
    let mut d = AacDecoder::new(&AacConfig { asc: s.config(), adts: s.p1 != 0, he, caps: psram(), scratch_caps: None, clock: Some(rust_clock) }).expect("aac");
    for _ in 0..ROUNDS {
        for packet in s.packets() {
            d.decode(packet, &mut pcm.s16).expect("decode");
        }
    }
    r_prof.copy_from_slice(&d.take_profile().expect("clock"));
    unsafe {
        let mut c: ffi::audf_aac_config_t = core::mem::zeroed();
        c.asc = s.config().as_ptr();
        c.asc_len = s.config().len();
        c.adts = s.p1 != 0;
        c.he = u32::from(s.p2) as _;
        c.alloc_caps = psram().bits();
        c.clock = Some(c_clock);
        let mut dec = ptr::null_mut();
        assert_eq!(ffi::audf_aac_decoder_create(&c, &mut dec), 0);
        for _ in 0..ROUNDS {
            for packet in s.packets() {
                let mut n = 0;
                ffi::audf_decoder_decode(dec, packet.as_ptr().cast(), packet.len(), pcm.s16.as_mut_ptr().cast(), &mut n);
            }
        }
        ffi::audf_aac_take_profile(dec, c_prof.as_mut_ptr());
        ffi::audf_decoder_destroy(dec);
    }
    for (stage, (c, r)) in ["core", "filterbank", "qmf analysis", "sbr", "ps", "qmf synthesis"].iter().zip(c_prof.iter().zip(&r_prof)) {
        info!(TAG, "{name} {stage}: C {c} us, Rust {r} us");
    }
}

fn bench() {
    match (aac::kernel_selftest(1, 200), mp3::kernel_selftest(1, 2000)) {
        (Ok(0), Ok(0)) => info!(TAG, "kernel selftests ok"),
        (a, m) => error!(TAG, "kernel selftests: aac {a:?}, mp3 {m:?}"),
    }
    let mut pcm = Pcm::new(6000 * 8);
    bench_dsp(&mut pcm);
    for (name, clip) in CLIPS {
        bench_clip(name, clip, &mut pcm);
        if name.starts_with("aac") {
            aac_profile(name, clip, &mut pcm);
        }
    }
    info!(TAG, "BENCH DONE");
}

pub fn run() {
    let task = thread::Builder::new().name("bench").stack_size(32 * 1024).core(Some(1)).stack_caps(Caps::SIMD).spawn(bench).expect("bench task");
    task.join();
    #[cfg(not(target_os = "none"))]
    idf::system::restart();
}
