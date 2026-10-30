/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::VecDeque;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::{Memory, bounded, c_path};
use crate::avi::AviDemuxer;
use crate::es::EsDemuxer;
use crate::flac::FlacDemuxer;
use crate::mkv::MkvDemuxer;
use crate::mp4::Mp4Demuxer;
use crate::ogg::OggDemuxer;
use crate::wav::WavDemuxer;
use crate::{Arena, AudioCodec, Container, CoverFormat, Demux, Demuxer, OpenOptions, PacketKind, Result, Tags, VideoCodec, open};

const ARENA_BYTES: usize = 4 * 1024 * 1024;
const SMALL_ARENA_BYTES: usize = 1024 * 1024;
const HELD_PACKETS: usize = 4;
const SEEK_READS: usize = 6;

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF29CE484222325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100000001B3))
}

fn video_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Mjpeg => "MJPEG",
        VideoCodec::H264 => "H264",
        VideoCodec::Mpeg2 => "MPEG2",
        VideoCodec::Unsupported => "unsupported",
    }
}

fn audio_name(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::AdpcmIma => "IMA",
        other => other.name(),
    }
}

fn extension(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

fn wrap<T>(result: Result<T>, kind: fn(Box<T>) -> Demux) -> Result<Demux> {
    result.map(|d| kind(Box::new(d)))
}

fn open_by_extension(path: &Path, name: &CStr, arena: Arena, options: &OpenOptions) -> Result<Demux> {
    match extension(path) {
        "avi" => wrap(AviDemuxer::open(name, arena, options), Demux::Avi),
        "wav" => wrap(WavDemuxer::open(name, arena, options), Demux::Wav),
        "mp3" | "mp2" | "aac" => wrap(EsDemuxer::open(name, arena, options), Demux::Es),
        "flac" => wrap(FlacDemuxer::open(name, arena, options), Demux::Flac),
        "opus" => wrap(OggDemuxer::open(name, arena, options), Demux::Ogg),
        "mkv" | "webm" => wrap(MkvDemuxer::open(name, arena, options), Demux::Mkv),
        _ => wrap(Mp4Demuxer::open(name, arena, options), Demux::Mp4),
    }
}

fn print_info(out: &mut String, d: &Demux) {
    let info = d.info();
    let name = match info.container {
        Container::Avi => "avi",
        Container::Wav => "wav",
        Container::Es => "es",
        Container::Flac => "flac",
        Container::Ogg => "ogg",
        Container::Mkv => "mkv",
        Container::Mp4 => "mp4",
    };
    writeln!(out, "container {name}").unwrap();
    if let Some(v) = &info.video {
        writeln!(out, "video {} {} {} {} {} {} {} {}:{:016x}", video_name(v.codec), v.width, v.height, v.frame_interval_us,
                 v.rotation_ccw, v.frame_count, v.max_frame_bytes, v.codec_private.len(), fnv(&v.codec_private)).unwrap();
    }
    if let Some(a) = &info.audio {
        writeln!(out, "audio {} {} {} {} {} {} {} {}:{:016x}", audio_name(a.codec), a.sample_rate, a.bitrate_bps, a.channels,
                 a.bits_per_sample, a.block_align, a.max_frame_bytes, a.codec_private.len(), fnv(&a.codec_private)).unwrap();
    }
    writeln!(out, "time {} {} {}", info.duration_us, i32::from(info.duration_exact), i32::from(info.seekable)).unwrap();
}

fn print_tags(out: &mut String, t: &Tags) {
    writeln!(out, "tags [{}] [{}] [{}] [{}] [{}] [{}]", t.title.as_str(), t.artist.as_str(), t.album.as_str(),
             t.album_artist.as_str(), t.track.as_str(), t.date.as_str()).unwrap();
    let (size, format, front, hash) = match &t.cover {
        Some(cover) => {
            let format = match cover.format {
                CoverFormat::Jpeg => 1,
                CoverFormat::Png => 2,
            };
            (cover.len(), format, i32::from(cover.front), fnv(cover))
        }
        None => (0, 0, 0, fnv(&[])),
    };
    let (offset, at_size) = t.cover_at.map_or((0, 0), |at| (at.offset, at.size));
    writeln!(out, "cover {size} {format} {front} {hash:016x} at {offset} {at_size} scanned {}", i32::from(t.cover_scanned)).unwrap();
}

fn print_packet(out: &mut String, container: Container, p: &crate::Packet) {
    let kind = if p.kind == PacketKind::Video { 'V' } else { 'A' };
    let pts = if container == Container::Avi { i64::from(p.frame) } else { p.pts_us };
    writeln!(out, "{kind} {pts} {} {} {:016x}", i32::from(p.keyframe), p.data.len(), fnv(&p.data)).unwrap();
}

fn read_all(out: &mut String, d: &mut Demux, hold: usize) {
    let container = d.info().container;
    let mut held = VecDeque::new();
    let mut packets = 0;
    while let Ok(Some(p)) = d.read() {
        print_packet(out, container, &p);
        packets += 1;
        if hold != 0 {
            if held.len() == hold {
                held.pop_front();
            }
            held.push_back(p);
        }
    }
    drop(held);
    writeln!(out, "end {packets}").unwrap();
}

fn seek_raw(d: &mut Demux, target: i64) -> Result<i64> {
    match d {
        Demux::Avi(avi) => avi.seek_frame(target as u32).map(i64::from),
        other => other.seek(target),
    }
}

fn seek_to(out: &mut String, d: &mut Demux, target: i64) {
    let result = seek_raw(d, target);
    writeln!(out, "seek {target} {} {}", i32::from(result.is_ok()), result.unwrap_or(0)).unwrap();
    if result.is_err() {
        return;
    }
    let container = d.info().container;
    for _ in 0..SEEK_READS {
        match d.read() {
            Ok(Some(p)) => print_packet(out, container, &p),
            _ => break,
        }
    }
}

fn span_of(d: &Demux) -> i64 {
    match d {
        Demux::Avi(_) => i64::from(d.info().video.as_ref().unwrap().frame_count),
        _ => d.info().duration_us,
    }
}

fn seeks(out: &mut String, d: &mut Demux) {
    let span = span_of(d);
    let beyond = if matches!(d, Demux::Avi(_)) { 1000 } else { 1_000_000 };
    for target in [-1, 0, 1] {
        seek_to(out, d, target);
    }
    for k in 1..=7 {
        seek_to(out, d, span.wrapping_mul(k) / 8);
    }
    for target in [span - 1, span, span + beyond, span / 3, 0] {
        seek_to(out, d, target);
    }
}

fn keyframes(out: &mut String, d: &mut Demux) {
    let span = span_of(d);
    for k in 0..=8 {
        let target = span.wrapping_mul(k) / 8;
        let (key, at) = match d {
            Demux::Avi(avi) => {
                let frame = target as u32;
                (avi.keyframe_before(frame).map(i64::from), avi.keyframe_at(frame))
            }
            Demux::Mkv(mkv) => (mkv.keyframe_before(target), mkv.keyframe_at(target)),
            Demux::Mp4(mp4) => (mp4.keyframe_before(target), mp4.keyframe_at(target)),
            _ => unreachable!(),
        };
        let (offset, size) = at.unwrap_or((0, 0));
        writeln!(out, "key {target} {} {} {} {offset} {size}", i32::from(key.is_some()), key.unwrap_or(0), i32::from(at.is_some())).unwrap();
    }
    let _ = seek_raw(d, 0);
}

fn scenario(out: &mut String, path: &Path, bytes: usize, direct: bool, cover: bool, full: bool, hold: usize, name: &str) {
    let memory = Memory::new(bytes);
    let arena = if direct { Arena::direct(memory) } else { Arena::new(memory) };
    writeln!(out, "== {name}").unwrap();
    let mut d = match open_by_extension(path, &c_path(path), arena, &OpenOptions { want_cover: cover }) {
        Ok(d) => d,
        Err(e) => {
            writeln!(out, "failed [{e}]").unwrap();
            return;
        }
    };
    print_info(out, &d);
    print_tags(out, &d.info().tags);
    if direct {
        let got = matches!(d.read(), Ok(Some(_)));
        writeln!(out, "direct read {}", i32::from(got)).unwrap();
        return;
    }
    read_all(out, &mut d, hold);
    if !full {
        return;
    }
    seeks(out, &mut d);
    let info = d.info();
    let has_video = info.video.is_some();
    if has_video {
        keyframes(out, &mut d);
    }
    if has_video && d.info().audio.is_some() {
        d.set_want_audio(false);
        read_all(out, &mut d, 0);
        seeks(out, &mut d);
    }
}

fn dump(path: &Path) -> String {
    let mut out = String::new();
    scenario(&mut out, path, ARENA_BYTES, false, true, true, 0, "full");
    scenario(&mut out, path, SMALL_ARENA_BYTES, false, true, false, HELD_PACKETS, "small");
    scenario(&mut out, path, ARENA_BYTES, false, false, false, 0, "nocover");
    scenario(&mut out, path, ARENA_BYTES, true, false, false, 0, "direct");
    out
}

fn material() -> Option<Vec<PathBuf>> {
    let dir = std::env::var_os("DEVKIT_TEST_FIXTURES")?;
    let mut files: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.retain(|p| p.is_file() && !matches!(extension(p), "cdump" | "rdump" | ""));
    files.sort();
    Some(files)
}

#[test]
fn matches_media_framework() {
    let Some(files) = material() else {
        return;
    };
    assert!(!files.is_empty());
    let only = std::env::var("DEVKIT_MEDIA_ONLY").ok();
    let mut failed = 0;
    let mut compared = 0;
    for path in &files {
        if only.as_deref().is_some_and(|only| !path.to_str().unwrap().contains(only)) {
            continue;
        }
        let expected = path.with_extension(format!("{}.cdump", extension(path)));
        let Ok(expected) = fs::read_to_string(&expected) else {
            println!("no C dump for {}", path.display());
            continue;
        };
        let name = path.to_path_buf();
        let actual = bounded(120, move || dump(&name));
        compared += 1;
        if actual == expected {
            continue;
        }
        failed += 1;
        fs::write(path.with_extension(format!("{}.rdump", extension(path))), &actual).unwrap();
        let line = actual.lines().zip(expected.lines()).position(|(a, b)| a != b).unwrap_or(actual.lines().count().min(expected.lines().count()));
        println!("DIFF {} line {}:\n  rust: {}\n  c:    {}", path.file_name().unwrap().to_str().unwrap(), line + 1,
                 actual.lines().nth(line).unwrap_or("<end>"), expected.lines().nth(line).unwrap_or("<end>"));
    }
    println!("compared {compared}, {failed} differ");
    assert_eq!(failed, 0);
    assert!(only.is_some() || compared == files.len(), "compared {compared} of {}", files.len());
}

#[test]
fn open_picks_the_container() {
    let Some(files) = material() else {
        return;
    };
    for path in &files {
        let stem = path.file_name().unwrap().to_str().unwrap();
        if stem.starts_with("cut") || stem.starts_with("stomp") {
            continue;
        }
        let want = match extension(path) {
            "avi" => Container::Avi,
            "wav" => Container::Wav,
            "mp3" | "mp2" | "aac" => Container::Es,
            "flac" => Container::Flac,
            "opus" => Container::Ogg,
            "mkv" | "webm" => Container::Mkv,
            _ => Container::Mp4,
        };
        let name = c_path(path);
        if let Ok(d) = open(&name, Arena::new(Memory::new(ARENA_BYTES)), &OpenOptions::default()) {
            assert_eq!(d.info().container, want, "{}", path.display());
        }
    }
}

#[test]
fn survives_damaged_files() {
    let Some(files) = material() else {
        return;
    };
    let rounds: u32 = std::env::var("DEVKIT_MEDIA_MUTATIONS").ok().and_then(|n| n.parse().ok()).unwrap_or(1);
    let dir = std::env::temp_dir().join(format!("devkit_media_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for path in files.iter().filter(|p| fs::metadata(p).unwrap().len() < 400_000) {
        let name = path.file_name().unwrap().to_str().unwrap();
        if name.starts_with("cut") || name.starts_with("stomp") {
            continue;
        }
        let data = fs::read(path).unwrap();
        for round in 0..rounds {
            let mut state = name.bytes().fold(u64::from(round) * 2 + 1, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as usize
            };
            let mut bytes = data.clone();
            let head = bytes.len().min(4096);
            for _ in 0..24 {
                let at = if next() % 2 == 0 { next() % head } else { next() % bytes.len() };
                bytes[at] = next() as u8;
            }
            let target = dir.join(name);
            fs::write(&target, &bytes).unwrap();
            bounded(60, move || {
                let mut d = match open_by_extension(&target, &c_path(&target), Arena::new(Memory::new(SMALL_ARENA_BYTES)), &OpenOptions { want_cover: true }) {
                    Ok(d) => d,
                    Err(_) => return,
                };
                let mut packets = 0;
                while let Ok(Some(_)) = d.read() {
                    packets += 1;
                    if packets > 3000 {
                        break;
                    }
                }
                let duration = d.info().duration_us;
                for target in [0, duration / 2, duration.wrapping_add(1000)] {
                    if d.seek(target).is_ok() {
                        let _ = d.read();
                    }
                }
            });
        }
    }
    let _ = fs::remove_dir_all(dir);
}
