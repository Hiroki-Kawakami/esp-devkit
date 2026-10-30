/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::fs;
use std::path::{Path, PathBuf};

use idf::heap::Caps;

use crate::aac::{AacConfig, AacDecoder, HeMode};
use crate::alac::{AlacConfig, AlacDecoder};
use crate::flac::{FlacConfig, FlacDecoder};
use crate::mp3::{Mp3Config, Mp3Decoder};
use crate::opus::{OpusConfig, OpusDecoder};
use crate::{Decoder, Format, PcmMut};

fn u32le(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

struct Stream<'a> {
    codec: u8,
    p1: u8,
    p2: u8,
    config: &'a [u8],
    packets: Vec<&'a [u8]>,
}

fn parse(file: &[u8]) -> Stream<'_> {
    assert_eq!(&file[..4], b"PKT1");
    let cfg_len = u32le(&file[8..]) as usize;
    let config = &file[12..12 + cfg_len];
    let mut at = 12 + cfg_len;
    let count = u32le(&file[at..]) as usize;
    at += 4;
    let mut packets = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u32le(&file[at..]) as usize;
        packets.push(&file[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    Stream { codec: file[4], p1: file[5], p2: file[6], config, packets }
}

enum Dut {
    Aac(AacDecoder),
    Mp3(Mp3Decoder),
    Opus(OpusDecoder),
    Flac(FlacDecoder),
    Alac(AlacDecoder),
}

impl Dut {
    fn open(s: &Stream<'_>) -> crate::Result<Self> {
        let caps = Caps::DEFAULT;
        Ok(match s.codec {
            0 => {
                let he = [HeMode::Off, HeMode::V1, HeMode::V2][usize::from(s.p2)];
                Dut::Aac(AacDecoder::new(&AacConfig { asc: s.config, adts: s.p1 != 0, he, caps, scratch_caps: None, clock: None })?)
            }
            1 => Dut::Mp3(Mp3Decoder::new(&Mp3Config { channels: s.p1, caps })?),
            2 => Dut::Opus(OpusDecoder::new(&OpusConfig { head: s.config, caps })?),
            3 => Dut::Flac(FlacDecoder::new(&FlacConfig { streaminfo: s.config, caps })?),
            _ => Dut::Alac(AlacDecoder::new(&AlacConfig { cookie: s.config, caps })?),
        })
    }

    fn decoder(&mut self) -> &mut dyn Decoder {
        match self {
            Dut::Aac(d) => d,
            Dut::Mp3(d) => d,
            Dut::Opus(d) => d,
            Dut::Flac(d) => d,
            Dut::Alac(d) => d,
        }
    }

    fn range(&self) -> u32 {
        if let Dut::Opus(d) = self { d.final_range() } else { 0 }
    }
}

fn run(pkt: &Path) -> Result<usize, String> {
    let file = fs::read(pkt).map_err(|e| e.to_string())?;
    let reference = fs::read(pkt.with_extension("ref")).map_err(|e| e.to_string())?;
    let s = parse(&file);
    let created = u32le(&reference) != 0;
    let mut dut = match Dut::open(&s) {
        Ok(dut) if created => dut,
        Err(_) if !created => return Ok(0),
        Ok(_) => return Err("C refused the stream".into()),
        Err(e) => return Err(format!("create: {e}")),
    };
    let (format, channels, max) = {
        let d = dut.decoder();
        (d.format(), usize::from(d.channels()), d.max_frames())
    };
    let mut s16 = vec![0i16; max * channels];
    let mut s32 = vec![0i32; max * channels];
    let mut at = 4;
    let mut samples = 0;
    let passes = [s.packets.len(), s.packets.len().min(64)];
    for (pass, count) in passes.into_iter().enumerate() {
        if pass == 1 {
            dut.decoder().reset();
        }
        for (i, packet) in s.packets[..count].iter().enumerate() {
            let pcm = match format {
                Format::S16 => PcmMut::S16(&mut s16),
                Format::S32 => PcmMut::S32(&mut s32),
            };
            let result = dut.decoder().decode(packet, pcm);
            let (ok, frames, range) = (u32le(&reference[at..]) != 0, u32le(&reference[at + 4..]) as usize, u32le(&reference[at + 8..]));
            at += 12;
            let got = result.as_ref().copied().unwrap_or(0);
            if result.is_ok() != ok || got != frames {
                return Err(format!("pass {pass} packet {i}: {result:?}, C ok={ok} frames={frames}"));
            }
            if dut.range() != range {
                return Err(format!("pass {pass} packet {i}: range {:#x}, C {range:#x}", dut.range()));
            }
            let n = frames * channels;
            let same = match format {
                Format::S16 => {
                    let r = &reference[at..at + 2 * n];
                    let mut first = None;
                    for k in 0..n {
                        if s16[k] != i16::from_le_bytes([r[2 * k], r[2 * k + 1]]) {
                            first = Some(k);
                            break;
                        }
                    }
                    at += 2 * n;
                    first
                }
                Format::S32 => {
                    let r = &reference[at..at + 4 * n];
                    let mut first = None;
                    for k in 0..n {
                        if s32[k] != i32::from_le_bytes([r[4 * k], r[4 * k + 1], r[4 * k + 2], r[4 * k + 3]]) {
                            first = Some(k);
                            break;
                        }
                    }
                    at += 4 * n;
                    first
                }
            };
            if let Some(k) = same {
                return Err(format!("pass {pass} packet {i} sample {k} differs"));
            }
            samples += n;
        }
    }
    if at != reference.len() {
        return Err("reference has more data".into());
    }
    Ok(samples)
}

#[test]
fn decoders_match_the_c_reference() {
    let Some(dir) = std::env::var("DEVKIT_TEST_FIXTURES").ok() else {
        eprintln!("DEVKIT_TEST_FIXTURES not set: run test/run.sh");
        return;
    };
    let mut files: Vec<PathBuf> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "pkt")).collect();
    files.sort();
    assert!(!files.is_empty());
    let mut failed = 0;
    for f in &files {
        let name = f.file_stem().unwrap().to_string_lossy();
        match run(f) {
            Ok(samples) => println!("ok   {name}: {samples} samples"),
            Err(e) => {
                println!("FAIL {name}: {e}");
                failed += 1;
            }
        }
    }
    println!("{} of {} streams match", files.len() - failed, files.len());
    assert_eq!(failed, 0);
}
