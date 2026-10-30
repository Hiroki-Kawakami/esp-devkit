/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod diff;

use std::f64::consts::PI;
use std::thread;
use std::time::Duration;

use idf::heap::Caps;

use crate::adpcm::{AdpcmConfig, AdpcmDecoder, AdpcmEncoder};
use crate::alac::{ALAC_COOKIE_BYTES, AlacConfig, AlacDecoder};
use crate::convert::{self, PcmLayout};
use crate::drift::{Drift, DriftConfig};
use crate::eq::{Biquad, Eq, EqConfig};
use crate::fifo::{self, FifoConfig, Overrun, Underrun};
use crate::flac::{FLAC_STREAMINFO_BYTES, FlacConfig, FlacDecoder};
use crate::gain::{GAIN_MAX, Gain, GainConfig};
use crate::graph::{GraphBuilder, GraphConfig};
use crate::mixer::{Mixer, MixerConfig};
use crate::mp3::{self, MP3_MAX_FRAMES, Mp3Config, Mp3Decoder};
use crate::resampler::{Resampler, ResamplerConfig, ResamplerKind};
use crate::{Decoder, Encoder, Error, Format, Pcm, PcmMut};

fn sine16(buf: &mut [i16], channels: usize, freq: f64, rate: f64, amp: f64, phase: &mut f64) {
    for frame in buf.chunks_exact_mut(channels) {
        frame.fill((amp * phase.sin()).round() as i16);
        *phase += 2.0 * PI * freq / rate;
    }
}

// Amplitude of `freq` and the RMS of everything else, in dB below it.
fn tone_purity_db(x: &[i16], stride: usize, freq: f64, rate: f64) -> (f64, f64) {
    let n = x.len() / stride;
    let (mut s, mut c) = (0f64, 0f64);
    for i in 0..n {
        let w = 2.0 * PI * freq / rate * i as f64;
        s += f64::from(x[i * stride]) * w.sin();
        c += f64::from(x[i * stride]) * w.cos();
    }
    let a = 2.0 * (s * s + c * c).sqrt() / n as f64;
    let ph = c.atan2(s);
    let mut err = 0f64;
    for i in 0..n {
        let w = 2.0 * PI * freq / rate * i as f64;
        let d = f64::from(x[i * stride]) - a * (w + ph).sin();
        err += d * d;
    }
    let rms = (err / n as f64).sqrt().max(1e-9);
    (20.0 * (a / core::f64::consts::SQRT_2 / rms).log10(), a)
}

#[test]
fn convert_layouts() {
    let mut s32 = [0i32; 3];
    convert::from_pcm(&[0x00, 0x01, 0xff, 0x7f, 0x00, 0x80], PcmLayout::S16, &mut s32);
    assert_eq!(s32, [256 * 256, 32767 * 256, -32768 * 256]);
    let mut bytes = [0u8; 6];
    convert::to_pcm(&s32, &mut bytes, PcmLayout::S16);
    assert_eq!(bytes, [0x00, 0x01, 0xff, 0x7f, 0x00, 0x80]);

    let mut s24 = [0u8; 6];
    convert::to_pcm(&[1 << 24, -(1 << 24)], &mut s24, PcmLayout::S24);
    assert_eq!(s24, [0xff, 0xff, 0x7f, 0x00, 0x00, 0x80]);
    let mut back = [0i32; 2];
    convert::from_pcm(&s24, PcmLayout::S24, &mut back);
    assert_eq!(back, [(1 << 23) - 1, -(1 << 23)]);

    let mut s32le = [0u8; 4];
    convert::to_pcm(&[0x123456], &mut s32le, PcmLayout::S32);
    assert_eq!(i32::from_le_bytes(s32le), 0x12345600);

    let mut wide = [0i32; 2];
    convert::convert(&[100i16, -100], &mut wide);
    assert_eq!(wide, [25600, -25600]);
    let mut narrow = [0i16; 2];
    convert::convert(&[1i32 << 24, 127], &mut narrow);
    assert_eq!(narrow, [32767, 0]);
}

fn eq<S: crate::Sample>(channels: u8) -> (Eq<S>, crate::eq::EqControl) {
    Eq::new(&EqConfig { channels, enabled: true, ..Default::default() }).unwrap()
}

#[test]
fn eq_filters() {
    let fs = 48000;
    let (mut e, ctl) = eq::<i16>(1);
    let mut buf: Vec<i16> = (0..256).map(|i| (i * 257 - 32768) as i16).collect();
    let reference = buf.clone();
    e.process_in_place(&mut buf);
    assert_eq!(buf, reference, "no stages is a bit-exact bypass");

    ctl.set_biquads(&[Biquad::lowpass(fs, 1000.0, 0.707)]).unwrap();
    let mut x = vec![8000i16; 2000];
    e.process_in_place(&mut x);
    assert!((i32::from(x[1999]) - 8000).abs() < 20, "DC passes");
    for (i, v) in x.iter_mut().enumerate() {
        *v = if i & 1 != 0 { -8000 } else { 8000 };
    }
    e.process_in_place(&mut x);
    assert!(x[1000..].iter().all(|v| v.abs() < 100), "Nyquist attenuated");

    ctl.set_enabled(false);
    let mut raw = [8000i16, -8000];
    e.process_in_place(&mut raw);
    assert_eq!(raw, [8000, -8000], "disabled is a bypass");
    assert!(!ctl.enabled());

    let (mut e, ctl) = eq::<i16>(1);
    ctl.set_biquads(&[Biquad::low_shelf(fs, 10000.0, 0.707, 12.0)]).unwrap();
    let mut buf: Vec<i16> = (0..256).map(|i| if i / 64 % 2 != 0 { -30000 } else { 30000 }).collect();
    e.process_in_place(&mut buf);
    assert!(buf.iter().any(|v| i32::from(*v).abs() > 30000), "shelf boosts and saturates instead of wrapping");

    let pk = Biquad::peaking(fs, 200.0, 1.0, 6.0);
    let (mut a, actl) = eq::<i16>(2);
    let (mut b, bctl) = eq::<i16>(2);
    actl.set_biquads(&[pk]).unwrap();
    bctl.set_biquads(&[pk]).unwrap();
    let mut sa = vec![0i16; 2000];
    let mut ph = 0.0;
    sine16(&mut sa, 2, 300.0, f64::from(fs), 12000.0, &mut ph);
    let mut sb = sa.clone();
    a.process_in_place(&mut sa);
    let (first, second) = sb.split_at_mut(1000);
    b.process_in_place(first);
    bctl.set_biquads(&[pk]).unwrap();
    b.process_in_place(second);
    assert_eq!(sa, sb, "coefficient update keeps state");

    let (mut e16, c16) = eq::<i16>(1);
    let (mut e32, c32) = eq::<i32>(1);
    c16.set_biquads(&[pk]).unwrap();
    c32.set_biquads(&[pk]).unwrap();
    let mut m16 = vec![0i16; 1000];
    let mut ph = 0.0;
    sine16(&mut m16, 1, 300.0, f64::from(fs), 8000.0, &mut ph);
    let mut m32 = vec![0i32; 1000];
    convert::convert(&m16, &mut m32);
    let mut o16 = vec![0i16; 1000];
    let mut o32 = vec![0i32; 1000];
    e16.process(&m16, &mut o16);
    e32.process(&m32, &mut o32);
    let worst = (0..1000).map(|i| (i32::from(o16[i]) - (f64::from(o32[i]) / 256.0).round() as i32).abs()).max();
    assert!(worst.unwrap() <= 1, "S16 and S32 agree within an LSB");

    assert_eq!(c16.set_biquads(&[pk; 9]), Err(Error::InvalidSize));
    assert!(e16.reconfig(2).is_ok());
    assert_eq!(e16.channels(), 2);
    assert_eq!(e16.reconfig(9).err(), Some(Error::InvalidArg));
}

fn gain(rate: u32, channels: u8, g: f32) -> (Gain<i16>, crate::gain::GainControl) {
    Gain::new(&GainConfig { channels, sample_rate: rate, gain: g, caps: Caps::DEFAULT }).unwrap()
}

#[test]
fn gain_fades() {
    let (mut g, ctl) = gain(1000, 1, 1.0);
    let mut buf: Vec<i16> = (0..200).map(|i| i * 100).collect();
    let reference = buf.clone();
    g.process_in_place(&mut buf);
    assert_eq!(buf, reference, "unity is a bypass");

    ctl.set(0.0, 0);
    ctl.set(1.0, 100);
    assert!((ctl.get() - 1.0).abs() < 1e-6);
    let mut buf = [10000i16; 200];
    g.process_in_place(&mut buf);
    assert!(buf[0] < 500, "fade starts from the jump");
    assert!(buf[..100].windows(2).all(|w| w[1] >= w[0]), "monotonic");
    assert!(buf[100..].iter().all(|&v| v == 10000), "lands on target");

    let (mut a, actl) = gain(1000, 1, 0.0);
    let (mut b, bctl) = gain(1000, 1, 0.0);
    actl.set(0.8, 500);
    bctl.set(0.8, 500);
    let mut one = vec![12345i16; 750];
    let mut chunked = one.clone();
    a.process_in_place(&mut one);
    let mut off = 0;
    for c in [13, 1, 257, 64, 199, 216] {
        b.process_in_place(&mut chunked[off..off + c]);
        off += c;
    }
    assert_eq!(one, chunked, "fade is chunk-invariant");

    let (mut g, ctl) = gain(48000, 1, 2.0);
    let mut boost = [1000i16, -1000, 30000, -30000];
    g.process_in_place(&mut boost);
    assert_eq!(boost, [2000, -2000, 32767, -32768], "boost saturates");
    ctl.set(1000.0, 0);
    assert_eq!(ctl.get(), GAIN_MAX);
    ctl.set(-1.0, 0);
    assert_eq!(ctl.get(), 0.0);
}

fn mixer(out_channels: u8, in_channels: &[u8]) -> (Mixer<i16>, crate::mixer::MixerControl) {
    Mixer::new(&MixerConfig { out_channels, in_channels, caps: Caps::DEFAULT }).unwrap()
}

#[test]
fn mixer_matrices() {
    let (mut m, ctl) = mixer(2, &[2]);
    let mut buf = [1000i16, -1000, 2000, 0, -500, -500, 32767, 32767];
    let reference = buf;
    m.process_in_place(&mut buf, &[]);
    assert_eq!(buf, reference, "identity is a bypass");
    ctl.set_matrix(0, &[0.5, 0.5, 0.5, 0.5]).unwrap();
    m.process_in_place(&mut buf, &[]);
    assert_eq!(buf, [0, 0, 1000, 1000, -500, -500, 32767, 32767]);
    let mut got = [0f32; 4];
    ctl.matrix(0, &mut got).unwrap();
    assert_eq!(got[1], 0.5);

    let (mut m, _) = mixer(2, &[2, 1]);
    let st = [100i16, 200, 300, 400];
    let mono = [10i16, 20];
    let mut out = [0i16; 4];
    m.process(&[Some(&st), Some(&mono)], &mut out);
    assert_eq!(out, [110, 210, 320, 420], "stereo + upmixed mono");
    m.process(&[Some(&st), None], &mut out);
    assert_eq!((out[0], out[3]), (100, 400), "None is silence");
    m.process(&[Some(&[30000; 4]), Some(&[30000; 2])], &mut out);
    assert_eq!(out[0], 32767, "sum saturates");

    let (mut down, _) = mixer(1, &[2]);
    let mut pcm = [100i16, 300, -100, -300];
    let mut out = [0i16; 2];
    down.process(&[Some(&pcm)], &mut out);
    assert_eq!(out, [200, -200], "N->1 averages");
    down.process_in_place(&mut pcm, &[]);
    assert_eq!(pcm[..2], [200, -200], "in place");
}

fn resampler(kind: ResamplerKind, in_rate: u32, out_rate: u32, channels: u8) -> (Resampler<i16>, crate::resampler::ResamplerControl) {
    Resampler::new(&ResamplerConfig { kind, channels, in_rate, out_rate, ..Default::default() }).unwrap()
}

fn run_push(r: &mut Resampler<i16>, input: &[i16], ch: usize, out: &mut [i16], chunk: usize) -> usize {
    let mut produced = 0;
    for c in input.chunks(chunk * ch) {
        let room = r.max_out(c.len() / ch).min(out.len() / ch - produced);
        produced += r.process(c, &mut out[produced * ch..(produced + room) * ch]).unwrap();
    }
    produced
}

fn check_tone(kind: ResamplerKind, in_rate: u32, out_rate: u32, freq: f64, min_db: f64) {
    const IN: usize = 24000;
    let mut input = vec![0i16; IN * 2];
    let mut out = vec![0i16; IN * 8 * 2];
    let mut ph = 0.0;
    sine16(&mut input, 2, freq, f64::from(in_rate), 16000.0, &mut ph);
    let (mut r, _) = resampler(kind, in_rate, out_rate, 2);
    let n = run_push(&mut r, &input, 2, &mut out, 480);
    let expect = IN as f64 * f64::from(out_rate) / f64::from(in_rate);
    assert!((n as f64 - expect).abs() < 100.0, "output length");
    let body = &out[256 * 2..(n - 256) * 2];
    let (db, amp) = tone_purity_db(body, 2, freq, f64::from(out_rate));
    assert!(db > min_db, "tone purity {db}");
    assert!((amp - 16000.0).abs() < 400.0, "amplitude {amp}");
    let (db, _) = tone_purity_db(&body[1..], 2, freq, f64::from(out_rate));
    assert!(db > min_db, "right channel {db}");
}

#[test]
fn resampler_tones() {
    check_tone(ResamplerKind::Cubic, 48000, 48048, 1000.0, 50.0);
    check_tone(ResamplerKind::Polyphase, 44100, 48000, 5000.0, 60.0);
    check_tone(ResamplerKind::Polyphase, 48000, 16000, 3000.0, 60.0);
    check_tone(ResamplerKind::Integer, 16000, 48000, 3000.0, 60.0);
    check_tone(ResamplerKind::Integer, 48000, 24000, 3000.0, 60.0);
}

#[test]
fn resampler_pull_and_chunks() {
    for (kind, in_rate, out_rate) in
        [(ResamplerKind::Cubic, 44100, 48000), (ResamplerKind::Polyphase, 44100, 48000), (ResamplerKind::Integer, 16000, 32000)]
    {
        let (mut r, ctl) = resampler(kind, in_rate, out_rate, 1);
        let mut input = vec![0i16; 2048];
        let mut out = vec![0i16; 1024];
        let mut ph = 0.0;
        for it in 0..200 {
            let want = 100 + (it * 37 % 300);
            if kind != ResamplerKind::Integer {
                ctl.set_adjust((it % 7) as f32 * 50.0 - 150.0).unwrap();
            }
            let need = r.frames_needed(want);
            sine16(&mut input[..need], 1, 440.0, 44100.0, 10000.0, &mut ph);
            assert_eq!(r.process(&input[..need], &mut out[..want]), Ok(want), "frames_needed is exact");
        }
    }

    let mut input = vec![0i16; 4000];
    let mut ph = 0.0;
    sine16(&mut input, 1, 700.0, 44100.0, 12000.0, &mut ph);
    let (mut a, _) = resampler(ResamplerKind::Polyphase, 44100, 48000, 1);
    let (mut b, _) = resampler(ResamplerKind::Polyphase, 44100, 48000, 1);
    let (mut o1, mut o2) = (vec![0i16; 8000], vec![0i16; 8000]);
    let na = run_push(&mut a, &input, 1, &mut o1, 1000);
    let nb = run_push(&mut b, &input, 1, &mut o2, 77);
    assert_eq!(na, nb);
    assert_eq!(o1[..na], o2[..nb], "chunk-invariant");

    let (mut r, ctl) = resampler(ResamplerKind::Cubic, 48000, 48000, 1);
    ctl.set_adjust(1000.0).unwrap();
    let mut total = 0;
    let (tmp_in, mut tmp_out) = (vec![0i16; 1024], vec![0i16; 1024]);
    for _ in 0..100 {
        let need = r.frames_needed(1000);
        r.process(&tmp_in[..need], &mut tmp_out[..1000]).unwrap();
        total += need;
    }
    assert!(total > 100090 && total < 100110, "adjust shifts consumption ({total})");
    let (_, ctl) = resampler(ResamplerKind::Integer, 16000, 48000, 1);
    assert_eq!(ctl.set_adjust(1.0), Err(Error::NotSupported));
}

fn fifo_of(capacity: usize, prefill: usize, underrun: Underrun, overrun: Overrun) -> (fifo::Writer<i16>, fifo::Reader<i16>) {
    fifo::new(&FifoConfig { channels: 1, capacity, prefill, underrun, overrun, caps: Caps::DEFAULT }).unwrap()
}

#[test]
fn fifo_silence_and_drop() {
    let (mut w, mut r) = fifo_of(8, 4, Underrun::Silence, Overrun::DropOldest);
    let data = [1i16, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let mut out = [9i16; 6];
    w.write(&data[..3]).unwrap();
    r.read(&mut out[..2]).unwrap();
    assert_eq!(out[..2], [0, 0], "silence until prefill");
    assert!(!r.primed());
    w.write(&data[3..4]).unwrap();
    assert!(r.primed());
    r.read(&mut out).unwrap();
    assert_eq!((out[0], out[3], out[4], out[5]), (1, 4, 0, 0), "underrun pads with silence");
    assert!(!r.primed(), "underrun re-arms the prefill");
    w.write(&data).unwrap();
    assert_eq!(r.level(), 8, "drop-oldest keeps the newest");
    r.read(&mut out[..2]).unwrap();
    assert_eq!(out[..2], [3, 4], "oldest dropped");
    w.flush();
    assert_eq!(r.level(), 0);
    assert!(!r.primed());
}

#[test]
fn fifo_blocks_and_aborts() {
    let (mut w, mut r) = fifo_of(4, 0, Underrun::Block, Overrun::Block);
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        w.write(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        w
    });
    let mut got = [0i16; 8];
    r.read(&mut got).unwrap();
    assert_eq!(got, [1, 2, 3, 4, 5, 6, 7, 8], "blocking read passes through a larger write");
    let w = t.join().unwrap();
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        w.abort();
    });
    assert_eq!(r.read(&mut got[..1]), Err(Error::InvalidState), "abort wakes a blocked reader");
    t.join().unwrap();
}

#[test]
fn drift_absorbs_clock_offset() {
    let rate = 48000;
    let (mut w, r) = fifo_of(4800, 960, Underrun::Silence, Overrun::DropOldest);
    let (rs, _) = resampler(ResamplerKind::Cubic, rate, rate, 1);
    let drift = Drift::new(&DriftConfig { sample_rate: rate, target_frames: 960, ..Default::default() });
    let mut g = GraphBuilder::new(&GraphConfig { max_frames: 480, caps: Caps::DEFAULT });
    let fn_ = g.add_fifo(r);
    let rn = g.add_resampler(fn_, rs, Some(drift));
    let mut g = g.build().unwrap();

    let mut block = vec![0i16; 480 * 2];
    let (mut produced, mut written, mut worst) = (0f64, 0usize, 0usize);
    for i in 0..12000 {
        produced += 480.0 * 1.00015;
        let n = produced as usize - written;
        w.write(&block[..n]).unwrap();
        written += n;
        if i > 1000 {
            worst = worst.max(w.level().abs_diff(960));
        }
        g.read(rn, &mut block[..480]).unwrap();
    }
    assert!((g.drift_ppm(rn).unwrap() - 150.0).abs() < 15.0, "drift settles on the clock offset");
    assert!(worst < 100, "level holds near target ({worst})");

    w.flush();
    g.read(rn, &mut block[..480]).unwrap();
    assert_eq!(g.drift_ppm(rn), Some(0.0), "flush resets the drift");
}

#[test]
fn adpcm_roundtrip() {
    let cfg = AdpcmConfig { channels: 2, block_align: 1024 };
    let spb = cfg.block_frames();
    assert_eq!(spb, 1017);
    let mut enc = AdpcmEncoder::new(&cfg).unwrap();
    let mut dec = AdpcmDecoder::new(&cfg).unwrap();
    assert_eq!(Encoder::frame_frames(&enc), spb);
    assert_eq!(Decoder::max_frames(&dec), spb);

    const BLOCKS: usize = 4;
    let mut pcm = vec![0i16; spb * 2 * BLOCKS];
    let mut ph = 0.0;
    sine16(&mut pcm, 2, 440.0, 44100.0, 12000.0, &mut ph);
    for f in pcm.chunks_exact_mut(2) {
        f[1] = f[0] / 2;
    }
    let mut decoded = vec![0i16; spb * 2 * BLOCKS];
    let mut block = [0u8; 1024];
    let mut total = 0;
    for b in 0..BLOCKS {
        let len = enc.encode(&pcm[b * spb * 2..(b + 1) * spb * 2], &mut block).unwrap();
        assert_eq!(len, 1024);
        total += dec.decode(&block[..len], &mut decoded[total * 2..]).unwrap();
    }
    assert_eq!(total, spb * BLOCKS);
    let (db, amp) = tone_purity_db(&decoded, 2, 440.0, 44100.0);
    assert!(db > 30.0 && (amp - 12000.0).abs() < 300.0, "left survives ({db}, {amp})");
    let (db, amp) = tone_purity_db(&decoded[1..], 2, 440.0, 44100.0);
    assert!(db > 25.0 && (amp - 6000.0).abs() < 200.0, "right is its own channel ({db}, {amp})");

    let len = enc.encode(&pcm[..40], &mut block).unwrap();
    assert_eq!(len, 8 * 4, "short block rounds up to whole groups");
    let frames = dec.decode(&block[..len], &mut decoded).unwrap();
    assert_eq!(frames, 25, "short block decodes padded");
    assert!((i32::from(decoded[19 * 2]) - i32::from(pcm[19 * 2])).abs() < 600, "last real frame survives");
    assert_eq!(dec.decode(&block[..13], &mut decoded), Err(Error::InvalidSize), "ragged length rejected");
    assert_eq!(Encoder::encode(&mut enc, Pcm::S32(&[0; 8]), &mut block), Err(Error::InvalidArg));
}

#[derive(Default)]
struct BitWriter {
    buf: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits / 8 == self.buf.len() {
                self.buf.push(0);
            }
            if v >> i & 1 != 0 {
                self.buf[self.bits / 8] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    fn bytes(&self) -> usize {
        self.bits.div_ceil(8)
    }
}

fn crc_bits(p: &[u8], width: u32, poly: u32) -> u32 {
    let top = 1u32 << (width - 1);
    let mask = (top << 1) - 1;
    let mut crc = 0u32;
    for &b in p {
        crc ^= u32::from(b) << (width - 8);
        for _ in 0..8 {
            crc = (if crc & top != 0 { crc << 1 ^ poly } else { crc << 1 }) & mask;
        }
    }
    crc
}

fn flac_streaminfo(channels: u8, bps: u8) -> [u8; FLAC_STREAMINFO_BYTES] {
    let mut si = [0u8; FLAC_STREAMINFO_BYTES];
    si[1] = 16;
    si[3] = 16;
    let rate = 44100u32;
    si[10] = (rate >> 12) as u8;
    si[11] = (rate >> 4) as u8;
    si[12] = ((rate & 15) << 4) as u8 | (channels - 1) << 1 | (bps - 1) >> 4;
    si[13] = (bps - 1) << 4;
    si
}

fn flac_frame(rate_code: u32, assignment: u32, size_code: u32, bps: u32, channels: &[&[i32; 16]]) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.put(0x3FFE, 14);
    w.put(0, 2);
    w.put(6, 4);
    w.put(rate_code, 4);
    w.put(assignment, 4);
    w.put(size_code, 3);
    w.put(0, 1);
    w.put(0, 8);
    w.put(15, 8);
    let crc = crc_bits(&w.buf, 8, 0x07);
    w.put(crc, 8);
    for (ch, samples) in channels.iter().enumerate() {
        let side = if assignment == 8 || assignment == 10 { ch == 1 } else { assignment == 9 && ch == 0 };
        w.put(1 << 1, 8);
        for &s in samples.iter() {
            let width = bps + u32::from(side);
            w.put(s as u32 & ((1u32 << width) - 1), width);
        }
    }
    w.bits = w.bits.div_ceil(8) * 8;
    w.buf.resize(w.bits / 8, 0);
    let crc = crc_bits(&w.buf, 16, 0x8005);
    w.put(crc, 16);
    w.buf
}

#[test]
fn flac_frames() {
    let si = flac_streaminfo(2, 32);
    assert_eq!(FlacDecoder::new(&FlacConfig { streaminfo: &si, caps: Caps::DEFAULT }).err(), Some(Error::NotSupported));

    let mut left = [0i32; 16];
    let mut right = [0i32; 16];
    for i in 0..16 {
        left[i] = i as i32 * 3001 - 24000;
        right[i] = 7 - i as i32 * 911;
    }
    let si = flac_streaminfo(2, 16);
    let mut dec = FlacDecoder::new(&FlacConfig { streaminfo: &si, caps: Caps::DEFAULT }).unwrap();
    assert_eq!(Decoder::format(&dec), Format::S16);
    assert_eq!((Decoder::max_frames(&dec), dec.rate()), (16, 44100));
    let mut frame = flac_frame(0, 1, 4, 16, &[&left, &right]);
    let mut pcm = [0i16; 32];
    assert_eq!(dec.decode_s16(&frame, &mut pcm), Ok(16));
    for i in 0..16 {
        assert_eq!((i32::from(pcm[i * 2]), i32::from(pcm[i * 2 + 1])), (left[i], right[i]));
    }
    pcm.fill(0x5a5a);
    let len = frame.len();
    frame[len / 2] ^= 0x10;
    assert_eq!(dec.decode_s16(&frame, &mut pcm), Err(Error::InvalidCrc));
    assert!(pcm.iter().all(|&v| v == 0x5a5a), "corrupt frame leaves pcm alone");
    frame[len / 2] ^= 0x10;
    assert_eq!(dec.decode_s16(&frame[..len - 1], &mut pcm), Err(Error::InvalidSize));
    assert_eq!(dec.decode_s16(&frame[..5], &mut pcm), Err(Error::InvalidSize));
    let other = flac_frame(10, 1, 4, 16, &[&left, &right]);
    assert_eq!(dec.decode_s16(&other, &mut pcm), Err(Error::NotSupported));

    let si = flac_streaminfo(2, 20);
    let mut dec = FlacDecoder::new(&FlacConfig { streaminfo: &si, caps: Caps::DEFAULT }).unwrap();
    assert_eq!(Decoder::format(&dec), Format::S32);
    let (mut mid, mut side) = ([0i32; 16], [0i32; 16]);
    for i in 0..16 {
        left[i] = i as i32 * 61001 - 500000;
        right[i] = 300001 - i as i32 * 37003;
        mid[i] = (left[i] + right[i]) >> 1;
        side[i] = left[i] - right[i];
    }
    let mut pcm32 = [0i32; 32];
    for (assignment, a, b) in [(8, &left, &side), (9, &side, &right), (10, &mid, &side)] {
        let frame = flac_frame(0, assignment, 5, 20, &[a, b]);
        assert_eq!(dec.decode_s32(&frame, &mut pcm32), Ok(16));
        for i in 0..16 {
            assert_eq!((pcm32[i * 2], pcm32[i * 2 + 1]), (left[i] * 16, right[i] * 16), "mode {assignment}");
        }
    }
}

fn alac_cookie(version: u8, bits: u8, channels: u8) -> [u8; ALAC_COOKIE_BYTES] {
    let mut c = [0, 0, 0, 16, 0, 16, 40, 10, 14, 2, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xac, 0x44];
    c[4] = version;
    c[5] = bits;
    c[9] = channels;
    c
}

fn alac_element(w: &mut BitWriter, tag: u32, frames: usize, bits: u32, channels: &[&[i32; 16]]) {
    w.put(tag, 3);
    w.put(0, 4);
    w.put(0, 12);
    w.put(if frames < 16 { 8 } else { 0 } | 1, 4);
    if frames < 16 {
        w.put(frames as u32, 32);
    }
    for i in 0..frames {
        for ch in channels {
            w.put(ch[i] as u32 & ((1u32 << bits) - 1), bits);
        }
    }
}

#[test]
fn alac_frames() {
    let open = |c: &[u8; ALAC_COOKIE_BYTES]| AlacDecoder::new(&AlacConfig { cookie: c, caps: Caps::DEFAULT });
    assert_eq!(open(&alac_cookie(0, 32, 2)).err(), Some(Error::NotSupported));
    assert_eq!(open(&alac_cookie(1, 16, 2)).err(), Some(Error::NotSupported));

    let mut left = [0i32; 16];
    let mut right = [0i32; 16];
    for i in 0..16 {
        left[i] = i as i32 * 3001 - 24000;
        right[i] = 7 - i as i32 * 911;
    }
    let cookie = alac_cookie(0, 16, 2);
    let mut dec = open(&cookie).unwrap();
    assert_eq!((Decoder::format(&dec), Decoder::channels(&dec), dec.rate()), (Format::S16, 2, 44100));
    let mut w = BitWriter::default();
    alac_element(&mut w, 1, 16, 16, &[&left, &right]);
    w.put(7, 3);
    let len = w.bytes();
    w.buf.resize(len, 0);
    let mut pcm = [0i16; 32];
    assert_eq!(dec.decode_s16(&w.buf, &mut pcm), Ok(16));
    for i in 0..16 {
        assert_eq!((i32::from(pcm[i * 2]), i32::from(pcm[i * 2 + 1])), (left[i], right[i]));
    }
    assert_eq!(dec.decode_s16(&w.buf[..len - 8], &mut pcm), Err(Error::InvalidSize));

    let mut w = BitWriter::default();
    alac_element(&mut w, 1, 10, 16, &[&left, &right]);
    assert_eq!(dec.decode_s16(&w.buf, &mut pcm), Ok(10), "partial frame");

    let mut w = BitWriter::default();
    w.put(7, 3);
    assert_eq!(dec.decode_s16(&w.buf, &mut pcm), Err(Error::NotSupported), "missing channels");
    let mut w = BitWriter::default();
    alac_element(&mut w, 0, 16, 16, &[&left]);
    alac_element(&mut w, 1, 16, 16, &[&left, &right]);
    assert_eq!(dec.decode_s16(&w.buf, &mut pcm), Err(Error::NotSupported), "extra channels");
    let mut w = BitWriter::default();
    w.put(2, 3);
    assert_eq!(dec.decode_s16(&w.buf, &mut pcm), Err(Error::InvalidData), "coupling element");

    let cookie = alac_cookie(0, 20, 1);
    let mut dec = open(&cookie).unwrap();
    assert_eq!(Decoder::format(&dec), Format::S32);
    for i in 0..16 {
        left[i] = i as i32 * 61001 - 500000;
    }
    let mut w = BitWriter::default();
    alac_element(&mut w, 0, 16, 20, &[&left]);
    let mut pcm32 = [0i32; 16];
    assert_eq!(dec.decode_s32(&w.buf, &mut pcm32), Ok(16));
    for i in 0..16 {
        assert_eq!(pcm32[i], left[i] * 16, "20-bit decodes at 24-bit scale");
    }
}

#[test]
fn mp3_headers_and_frames() {
    let h = mp3::parse_header(&[0xff, 0xfb, 0x90, 0x00]).unwrap();
    assert_eq!((h.layer, h.rate, h.channels, h.bytes, h.frames), (3, 44100, 2, 417, 1152));
    let h = mp3::parse_header(&[0xff, 0xfb, 0x92, 0xc0]).unwrap();
    assert_eq!((h.bytes, h.channels), (418, 1), "padding, mono");
    let h = mp3::parse_header(&[0xff, 0xf3, 0x80, 0x00]).unwrap();
    assert_eq!((h.rate, h.bytes, h.frames), (22050, 208, 576), "MPEG-2 Layer III");
    assert_eq!(mp3::parse_header(&[0xff, 0xe3, 0x80, 0x00]).unwrap().rate, 11025, "MPEG-2.5");
    let h = mp3::parse_header(&[0xff, 0xfd, 0xa4, 0xc0]).unwrap();
    assert_eq!((h.layer, h.rate, h.bytes, h.frames), (2, 48000, 576, 1152), "MPEG-1 Layer II");
    let h = mp3::parse_header(&[0xff, 0xf5, 0x80, 0x00]).unwrap();
    assert_eq!((h.bytes, h.frames), (417, 1152), "MPEG-2 Layer II");
    assert!(mp3::parse_header(&[0xff, 0xff, 0x90, 0x00]).is_none(), "Layer I refused");
    assert!(mp3::parse_header(&[0xff, 0xfb, 0x00, 0x00]).is_none(), "free format refused");
    assert!(mp3::parse_header(&[0xff, 0xe5, 0x80, 0x00]).is_none(), "MPEG-2.5 Layer II refused");

    assert_eq!(Mp3Decoder::new(&Mp3Config { channels: 3, caps: Caps::DEFAULT }).err(), Some(Error::InvalidArg));
    let mut dec = Mp3Decoder::new(&Mp3Config { channels: 2, caps: Caps::DEFAULT }).unwrap();
    let mut frame = vec![0u8; 576];
    let mut pcm = vec![0x5a5ai16; MP3_MAX_FRAMES * 2];
    frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
    assert_eq!(dec.decode(&frame[..417], &mut pcm), Ok(1152));
    assert!(pcm.iter().all(|&v| v == 0), "empty Layer III is silence");
    assert_eq!(dec.rate(), 44100);
    assert_eq!(dec.decode(&frame[..416], &mut pcm), Err(Error::InvalidSize));

    frame[4] = 0x80;
    dec.reset();
    assert_eq!(dec.decode(&frame[..417], &mut pcm), Ok(1152));
    assert_eq!(pcm[0], 0, "lost reservoir decodes as silence");

    frame.fill(0);
    frame[..4].copy_from_slice(&[0xff, 0xfd, 0xa4, 0xc0]);
    assert_eq!(dec.decode(&frame[..576], &mut pcm), Ok(1152), "mono Layer II into a stereo decoder");
    assert_eq!(dec.rate(), 48000);
    frame[..4].copy_from_slice(&[0xff, 0xff, 0x90, 0x00]);
    assert_eq!(dec.decode(&frame[..417], &mut pcm), Err(Error::NotSupported), "Layer I");
    frame[0] = 0;
    assert_eq!(dec.decode(&frame[..417], &mut pcm), Err(Error::InvalidData), "no sync");
}

fn graph_config(max_frames: usize) -> GraphConfig {
    GraphConfig { max_frames, caps: Caps::DEFAULT }
}

#[test]
fn graph_push_chain() {
    let pk = Biquad::peaking(48000, 1000.0, 1.0, 6.0);
    let (e, ctl) = eq::<i16>(2);
    ctl.set_biquads(&[pk]).unwrap();
    let (gn, _) = gain(48000, 2, 0.5);
    let mut captured = Vec::new();
    let mut src = vec![0i16; 2000];
    let mut ph = 0.0;
    sine16(&mut src, 2, 800.0, 48000.0, 10000.0, &mut ph);
    let reference = src.clone();
    {
        let mut g = GraphBuilder::new(&graph_config(256));
        let input = g.add_input::<i16>(2);
        let n = g.add_eq(input, e);
        let n = g.add_gain(n, gn);
        g.add_sink::<i16>(n, |pcm| {
            captured.extend_from_slice(pcm);
            Ok(())
        });
        let mut g = g.build().unwrap();
        g.write(input, &src).unwrap();
        assert_eq!(g.read(n, &mut [0i16; 20]), Err(Error::InvalidArg), "push node can't be read");
    }
    assert_eq!(src, reference, "caller's buffer is untouched");

    let (mut e2, ctl) = eq::<i16>(2);
    ctl.set_biquads(&[pk]).unwrap();
    let (mut g2, _) = gain(48000, 2, 0.5);
    let mut expect = reference;
    e2.process_in_place(&mut expect);
    g2.process_in_place(&mut expect);
    assert_eq!(captured, expect, "graph equals the modules run by hand");
}

struct Tone {
    phase: f64,
    freq: f64,
    remaining: usize,
}

impl Tone {
    fn read(&mut self, data: &mut [i16]) -> usize {
        let n = data.len().min(self.remaining);
        sine16(&mut data[..n], 1, self.freq, 48000.0, 8000.0, &mut self.phase);
        self.remaining -= n;
        n
    }
}

#[test]
fn graph_pull_mix() {
    let (m, _) = mixer(1, &[1, 1]);
    let mut tone = Tone { phase: 0.0, freq: 1000.0, remaining: 10000 };
    let (mut w, r) = fifo_of(1024, 0, Underrun::Silence, Overrun::Block);
    let mut g = GraphBuilder::new(&GraphConfig::default());
    let src = g.add_source::<i16>(1, |data| tone.read(data));
    let fn_ = g.add_fifo(r);
    let mix = g.add_mixer(&[src, fn_], m);
    let mut g = g.build().unwrap();
    w.write(&[1000i16; 100]).unwrap();
    let mut out = vec![0i16; 2000];
    g.read(mix, &mut out).unwrap();
    let mut reference = Tone { phase: 0.0, freq: 1000.0, remaining: 10000 };
    let mut expect = vec![0i16; 2000];
    reference.read(&mut expect);
    for (i, e) in expect.iter_mut().enumerate().take(100) {
        let _ = i;
        *e += 1000;
    }
    assert_eq!(out, expect, "source + FIFO, silence after the FIFO runs dry");
}

#[test]
fn graph_push_mixer_with_pulled_input() {
    let (m, _) = mixer(1, &[1, 1]);
    let (mut w, r) = fifo_of(4096, 480, Underrun::Silence, Overrun::DropOldest);
    let (rs, _) = resampler(ResamplerKind::Polyphase, 16000, 48000, 1);
    let drift = Drift::new(&DriftConfig { sample_rate: 16000, target_frames: 480, ..Default::default() });
    let mut captured = Vec::new();
    {
        let mut g = GraphBuilder::new(&graph_config(480));
        let bgm = g.add_input::<i16>(1);
        let fn_ = g.add_fifo(r);
        let rn = g.add_resampler(fn_, rs, Some(drift));
        let mix = g.add_mixer(&[bgm, rn], m);
        g.add_sink::<i16>(mix, |pcm| {
            captured.extend_from_slice(pcm);
            Ok(())
        });
        let mut g = g.build().unwrap();
        let mut mic = [0i16; 160];
        let music = [0i16; 480];
        let mut ph = 0.0;
        for _ in 0..16 {
            sine16(&mut mic, 1, 500.0, 16000.0, 4000.0, &mut ph);
            w.write(&mic).unwrap();
            g.write(bgm, &music).unwrap();
        }
    }
    assert_eq!(captured.len(), 16 * 480, "output paced by the pushed input");
    let (db, amp) = tone_purity_db(&captured[6 * 480..16 * 480], 1, 500.0, 48000.0);
    assert!(amp > 3500.0 && db > 30.0, "pulled input mixed after resampling ({amp}, {db})");
}

#[test]
fn graph_rules() {
    let tone = || |_: &mut [i16]| 0usize;
    let config = GraphConfig::default();

    let mut g = GraphBuilder::new(&config);
    let a = g.add_input::<i16>(1);
    let b = g.add_input::<i16>(1);
    g.add_mixer(&[a, b], mixer(1, &[1, 1]).0);
    assert_eq!(g.build().err(), Some(Error::InvalidArg), "two pushed inputs rejected");

    let mut g = GraphBuilder::new(&config);
    let s = g.add_source::<i16>(1, tone());
    g.add_sink::<i16>(s, |_| Ok(()));
    assert_eq!(g.build().err(), Some(Error::InvalidArg), "sink on a pull node rejected");

    let mut g = GraphBuilder::new(&config);
    let s = g.add_source::<i16>(1, tone());
    g.add_mixer(&[s, s], mixer(1, &[1, 1]).0);
    assert_eq!(g.build().err(), Some(Error::InvalidArg), "pull fan-out rejected");

    let (w, r) = fifo_of(64, 0, Underrun::Silence, Overrun::Block);
    let mut g = GraphBuilder::new(&config);
    let s = g.add_source::<i16>(1, tone());
    g.add_fifo_from(s, w, r);
    assert_eq!(g.build().err(), Some(Error::InvalidArg), "FIFO fed from a pull node rejected");

    let mut g = GraphBuilder::new(&config);
    let a = g.add_input::<i16>(2);
    g.add_mixer(&[a, a], mixer(1, &[1, 1]).0);
    assert_eq!(g.build().err(), Some(Error::InvalidArg), "channel mismatch rejected");

    let mut g = GraphBuilder::new(&config);
    let a = g.add_input::<i16>(1);
    let drift = Drift::new(&DriftConfig { sample_rate: 48000, target_frames: 10, ..Default::default() });
    g.add_resampler(a, resampler(ResamplerKind::Cubic, 48000, 48000, 1).0, Some(drift));
    assert_eq!(g.build().err(), Some(Error::InvalidArg), "drift without a FIFO rejected");

    let mut g = GraphBuilder::new(&config);
    let a = g.add_input::<i16>(1);
    g.add_convert(a, Format::S32);
    assert!(g.build().is_ok(), "convert builds");
}

#[test]
fn decoder_trait_rejects_the_wrong_format() {
    let cfg = AdpcmConfig { channels: 1, block_align: 256 };
    let mut dec = AdpcmDecoder::new(&cfg).unwrap();
    assert_eq!(Decoder::decode(&mut dec, &[0; 256], PcmMut::S32(&mut [0; 2])), Err(Error::InvalidArg));
}
