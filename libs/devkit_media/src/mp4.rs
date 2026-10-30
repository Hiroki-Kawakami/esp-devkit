/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::f64::consts::PI;

use devkit::math;
use idf::log::{info, warn};

use crate::alac;
use crate::buffer::{Aligned, Arena, Buffer};
use crate::demux::{AudioCodec, AudioTrack, Container, Demuxer, Info, OpenOptions, Packet, PacketKind, VideoCodec, VideoTrack};
use crate::error::{Error, Result};
use crate::flac;
use crate::tags::{Cover, Encoding, Field, Tags};

const TAG: &CStr = c"media_mp4";

const fn fourcc(id: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*id)
}

const BOX_MOOV: u32 = fourcc(b"moov");
const BOX_MOOF: u32 = fourcc(b"moof");
const BOX_MVHD: u32 = fourcc(b"mvhd");
const BOX_MVEX: u32 = fourcc(b"mvex");
const BOX_TRAK: u32 = fourcc(b"trak");
const BOX_TKHD: u32 = fourcc(b"tkhd");
const BOX_EDTS: u32 = fourcc(b"edts");
const BOX_ELST: u32 = fourcc(b"elst");
const BOX_MDIA: u32 = fourcc(b"mdia");
const BOX_MDHD: u32 = fourcc(b"mdhd");
const BOX_HDLR: u32 = fourcc(b"hdlr");
const BOX_MINF: u32 = fourcc(b"minf");
const BOX_STBL: u32 = fourcc(b"stbl");
const BOX_STSD: u32 = fourcc(b"stsd");
const BOX_STTS: u32 = fourcc(b"stts");
const BOX_CTTS: u32 = fourcc(b"ctts");
const BOX_STSS: u32 = fourcc(b"stss");
const BOX_STSC: u32 = fourcc(b"stsc");
const BOX_STSZ: u32 = fourcc(b"stsz");
const BOX_STZ2: u32 = fourcc(b"stz2");
const BOX_STCO: u32 = fourcc(b"stco");
const BOX_CO64: u32 = fourcc(b"co64");
const BOX_AVCC: u32 = fourcc(b"avcC");
const BOX_ESDS: u32 = fourcc(b"esds");
const BOX_WAVE: u32 = fourcc(b"wave");
const BOX_DOPS: u32 = fourcc(b"dOps");
const BOX_DFLA: u32 = fourcc(b"dfLa");
const BOX_ALAC: u32 = fourcc(b"alac");
const BOX_UDTA: u32 = fourcc(b"udta");
const BOX_META: u32 = fourcc(b"meta");
const BOX_ILST: u32 = fourcc(b"ilst");
const BOX_DATA: u32 = fourcc(b"data");
const BOX_NAM: u32 = fourcc(b"\xA9nam");
const BOX_ART: u32 = fourcc(b"\xA9ART");
const BOX_ALB: u32 = fourcc(b"\xA9alb");
const BOX_AART: u32 = fourcc(b"aART");
const BOX_DAY: u32 = fourcc(b"\xA9day");
const BOX_TRKN: u32 = fourcc(b"trkn");
const BOX_COVR: u32 = fourcc(b"covr");

const HANDLER_VIDEO: u32 = fourcc(b"vide");
const HANDLER_AUDIO: u32 = fourcc(b"soun");

const ENTRY_AVC1: u32 = fourcc(b"avc1");
const ENTRY_AVC3: u32 = fourcc(b"avc3");
const ENTRY_JPEG: u32 = fourcc(b"jpeg");
const ENTRY_MJPA: u32 = fourcc(b"mjpa");
const ENTRY_M2V1: u32 = fourcc(b"m2v1");
const ENTRY_MP4V: u32 = fourcc(b"mp4v");
const ENTRY_MP4A: u32 = fourcc(b"mp4a");
const ENTRY_MP3: u32 = fourcc(b".mp3");
const ENTRY_OPUS: u32 = fourcc(b"Opus");
const ENTRY_FLAC: u32 = fourcc(b"fLaC");
const ENTRY_ALAC: u32 = fourcc(b"alac");
const ENTRY_SOWT: u32 = fourcc(b"sowt");

const OTI_MPEG4_AUDIO: u8 = 0x40;
const OTI_MPEG2_VIDEO: core::ops::RangeInclusive<u8> = 0x60..=0x65;
const OTI_MPEG2_AAC: core::ops::RangeInclusive<u8> = 0x66..=0x68;
const OTI_MPEG2_MP3: u8 = 0x69;
const OTI_MPEG1_MP3: u8 = 0x6B;
const OTI_MJPEG: u8 = 0x6C;

const DESC_ES: u8 = 0x03;
const DESC_DECODER_CONFIG: u8 = 0x04;
const DESC_DECODER_INFO: u8 = 0x05;
const DECODER_CONFIG_BYTES: usize = 13;

const VISUAL_ENTRY_BYTES: usize = 78;
const AUDIO_ENTRY_BYTES: usize = 28;
const AUDIO_ENTRY_V1_BYTES: usize = 44;
const AUDIO_ENTRY_V2_BYTES: usize = 64;
const DOPS_BYTES: usize = 11;
const OPUS_HEAD_BYTES: usize = 19;

const MAX_MOOV_BYTES: u64 = 16 * 1024 * 1024;
const PCM_PACKET_BYTES: u32 = 32 * 1024;
const ROTATION_TOLERANCE_DEG: f64 = 1.0;

fn be16(p: &[u8], at: usize) -> u32 {
    match p.get(at..at + 2) {
        Some(b) => u32::from(b[0]) << 8 | u32::from(b[1]),
        None => 0,
    }
}

#[inline]
fn be32(p: &[u8], at: usize) -> u32 {
    match p.get(at..at + 4) {
        Some(b) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

fn be64(p: &[u8], at: usize) -> u64 {
    u64::from(be32(p, at)) << 32 | u64::from(be32(p, at + 4))
}

/// A byte range of `moov`, which the tracks keep pointing at for the life of
/// the demuxer.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

impl Span {
    fn len(self) -> usize {
        self.end - self.start
    }

    fn from(self, skip: usize) -> Self {
        Self { start: (self.start + skip).min(self.end), end: self.end }
    }

    fn bytes(self, moov: &[u8]) -> &[u8] {
        &moov[self.start..self.end]
    }
}

fn span_box(moov: &[u8], span: &mut Span) -> Option<(u32, Span)> {
    let left = span.len();
    if left < 8 {
        return None;
    }
    let mut size = u64::from(be32(moov, span.start));
    let mut header = 8;
    if size == 1 {
        if left < 16 {
            return None;
        }
        size = be64(moov, span.start + 8);
        header = 16;
    } else if size == 0 {
        size = left as u64;
    }
    if size < header as u64 || size > left as u64 {
        return None;
    }
    let kind = be32(moov, span.start + 4);
    let body = Span { start: span.start + header, end: span.start + size as usize };
    span.start = body.end;
    Some((kind, body))
}

fn span_find(moov: &[u8], mut span: Span, kind: u32) -> Option<Span> {
    while let Some((found, body)) = span_box(moov, &mut span) {
        if found == kind {
            return Some(body);
        }
    }
    None
}

fn ts_to_us(value: i64, timescale: u32) -> i64 {
    if timescale == 0 {
        return 0;
    }
    let timescale = i64::from(timescale);
    (value / timescale).wrapping_mul(1_000_000).wrapping_add((value % timescale).wrapping_mul(1_000_000) / timescale)
}

fn us_to_ts(us: i64, timescale: u32) -> i64 {
    let timescale = i64::from(timescale);
    (us / 1_000_000).wrapping_mul(timescale).wrapping_add((us % 1_000_000).wrapping_mul(timescale) / 1_000_000)
}

#[derive(Clone, Copy, Default)]
struct Table {
    at: Option<usize>,
    count: u32,
}

impl Table {
    fn load(moov: &[u8], body: Span, entry_bytes: usize, table: &mut Self) -> bool {
        if body.len() < 8 {
            return false;
        }
        let count = be32(moov, body.start + 4);
        if count as usize > (body.len() - 8) / entry_bytes {
            return false;
        }
        *table = Self { at: Some(body.start + 8), count };
        true
    }

    fn base(&self) -> usize {
        self.at.unwrap_or(usize::MAX / 2)
    }

    #[inline]
    fn word(&self, moov: &[u8], index: u32, stride: usize) -> u32 {
        be32(moov, self.base() + stride * index as usize)
    }

    fn run_count(&self, moov: &[u8], run: u32) -> u32 {
        self.word(moov, run, 8)
    }

    fn run_value(&self, moov: &[u8], run: u32) -> u32 {
        be32(moov, self.base() + 8 * run as usize + 4)
    }
}

#[derive(Clone, Copy)]
enum Codec {
    Video(VideoCodec),
    Audio(AudioCodec),
}

#[derive(Clone, Copy, Default)]
struct Track {
    handler: u32,
    enabled: bool,
    timescale: u32,
    duration: u64,
    matrix_a: i32,
    matrix_b: i32,
    elst: Option<Span>,
    stsd: Option<Span>,
    stts: Table,
    ctts: Table,
    stss: Table,
    stsc: Table,
    stco: Table,
    co64: bool,
    sizes: Option<usize>,
    constant_size: u32,
    size_bits: u8,
    sample_count: u32,
    fixed_size: u32,
    media_time: i64,
    delay_us: i64,
    codec: Option<Codec>,
    width: u32,
    height: u32,
    sample_rate: u32,
    bitrate_bps: u32,
    channels: u8,
    bits: u8,
    codec_private: Option<Span>,
    opus: bool,
}

#[derive(Clone, Copy, Default)]
struct SampleCursor {
    valid: bool,
    sample: u32,
    chunk: u32,
    chunk_left: u32,
    stsc_run: u32,
    offset: u64,
    stts_run: u32,
    stts_left: u32,
    dts: u64,
    ctts_run: u32,
    ctts_left: u32,
    sync_next: u32,
}

impl Track {
    fn load_sizes(&mut self, moov: &[u8], body: Span, compact: bool) {
        if body.len() < 12 {
            return;
        }
        let bits = if compact { moov[body.start + 7] } else { 32 };
        if !matches!(bits, 4 | 8 | 16 | 32) {
            return;
        }
        let constant = if compact { 0 } else { be32(moov, body.start + 4) };
        let count = be32(moov, body.start + 8);
        if constant == 0 && (u64::from(count) * u64::from(bits)).div_ceil(8) > (body.len() - 12) as u64 {
            return;
        }
        self.constant_size = constant;
        self.size_bits = bits;
        self.sizes = (constant == 0).then_some(body.start + 12);
        self.sample_count = count;
    }

    fn parse_stbl(&mut self, moov: &[u8], mut stbl: Span) {
        while let Some((kind, body)) = span_box(moov, &mut stbl) {
            match kind {
                BOX_STSD => self.stsd = Some(body),
                BOX_STTS => {
                    Table::load(moov, body, 8, &mut self.stts);
                }
                BOX_CTTS => {
                    Table::load(moov, body, 8, &mut self.ctts);
                }
                BOX_STSS => {
                    Table::load(moov, body, 4, &mut self.stss);
                }
                BOX_STSC => {
                    Table::load(moov, body, 12, &mut self.stsc);
                }
                BOX_STCO => {
                    if Table::load(moov, body, 4, &mut self.stco) {
                        self.co64 = false;
                    }
                }
                BOX_CO64 => {
                    if Table::load(moov, body, 8, &mut self.stco) {
                        self.co64 = true;
                    }
                }
                BOX_STSZ => self.load_sizes(moov, body, false),
                BOX_STZ2 => self.load_sizes(moov, body, true),
                _ => {}
            }
        }
    }

    fn parse_tkhd(&mut self, moov: &[u8], body: Span) {
        if body.len() < 4 {
            return;
        }
        self.enabled = be32(moov, body.start) & 1 != 0;
        let matrix = if moov[body.start] == 1 { 52 } else { 40 };
        if body.len() < matrix + 8 {
            return;
        }
        self.matrix_a = be32(moov, body.start + matrix) as i32;
        self.matrix_b = be32(moov, body.start + matrix + 4) as i32;
    }

    fn parse_mdhd(&mut self, moov: &[u8], body: Span) {
        let wide = moov[body.start] == 1;
        if body.len() < if wide { 32 } else { 20 } {
            return;
        }
        if wide {
            self.timescale = be32(moov, body.start + 20);
            self.duration = be64(moov, body.start + 24);
        } else {
            self.timescale = be32(moov, body.start + 12);
            self.duration = u64::from(be32(moov, body.start + 16));
        }
    }

    fn parse(moov: &[u8], trak: Span) -> Self {
        let mut track = Self::default();
        if let Some(body) = span_find(moov, trak, BOX_TKHD) {
            track.parse_tkhd(moov, body);
        }
        if let Some(edts) = span_find(moov, trak, BOX_EDTS) {
            track.elst = span_find(moov, edts, BOX_ELST);
        }
        let Some(mdia) = span_find(moov, trak, BOX_MDIA) else {
            return track;
        };
        if let Some(body) = span_find(moov, mdia, BOX_MDHD).filter(|body| body.len() >= 4) {
            track.parse_mdhd(moov, body);
        }
        if let Some(body) = span_find(moov, mdia, BOX_HDLR).filter(|body| body.len() >= 12) {
            track.handler = be32(moov, body.start + 8);
        }
        if let Some(stbl) = span_find(moov, mdia, BOX_MINF).and_then(|minf| span_find(moov, minf, BOX_STBL)) {
            track.parse_stbl(moov, stbl);
        }
        track
    }

    fn stsc_first(&self, moov: &[u8], run: u32) -> u32 {
        be32(moov, self.stsc.base() + 12 * run as usize)
    }

    fn stsc_spc(&self, moov: &[u8], run: u32) -> u32 {
        be32(moov, self.stsc.base() + 12 * run as usize + 4)
    }

    fn tables_valid(&self, moov: &[u8]) -> bool {
        if self.timescale == 0
            || self.sample_count == 0
            || self.stts.count == 0
            || self.stsc.count == 0
            || self.stco.count == 0
            || (self.constant_size == 0 && self.sizes.is_none())
        {
            return false;
        }
        if self.stsc_first(moov, 0) != 1 {
            return false;
        }
        for run in 0..self.stsc.count {
            let first = self.stsc_first(moov, run);
            if self.stsc_spc(moov, run) == 0 || first.wrapping_sub(1) >= self.stco.count {
                return false;
            }
            if run > 0 && first <= self.stsc_first(moov, run - 1) {
                return false;
            }
        }
        true
    }

    fn sample_size(&self, moov: &[u8], sample: u32) -> u32 {
        if self.fixed_size != 0 {
            return self.fixed_size;
        }
        if self.constant_size != 0 {
            return self.constant_size;
        }
        let sizes = self.sizes.unwrap_or(usize::MAX / 2);
        let index = sample as usize;
        match self.size_bits {
            32 => be32(moov, sizes + 4 * index),
            16 => be16(moov, sizes + 2 * index),
            8 => moov.get(sizes + index).map_or(0, |&b| u32::from(b)),
            _ => moov.get(sizes + index / 2).map_or(0, |&b| u32::from(b >> if sample & 1 != 0 { 0 } else { 4 }) & 0x0F),
        }
    }

    fn chunk_offset(&self, moov: &[u8], chunk: u32) -> u64 {
        if self.co64 {
            be64(moov, self.stco.base() + 8 * chunk as usize)
        } else {
            u64::from(be32(moov, self.stco.base() + 4 * chunk as usize))
        }
    }

    fn cursor_seek(&self, moov: &[u8], sample: u32) -> SampleCursor {
        let mut cursor = SampleCursor { sample, ..Default::default() };
        if sample >= self.sample_count {
            return cursor;
        }

        let mut base = 0u64;
        let mut index = 0;
        let mut found = false;
        for run in 0..self.stsc.count {
            let first = self.stsc_first(moov, run).wrapping_sub(1);
            let spc = self.stsc_spc(moov, run);
            let limit = if run + 1 < self.stsc.count { self.stsc_first(moov, run + 1).wrapping_sub(1) } else { self.stco.count };
            let chunks = u64::from(limit.min(self.stco.count).wrapping_sub(first));
            let run_samples = chunks.wrapping_mul(u64::from(spc));
            if u64::from(sample) < base.wrapping_add(run_samples) {
                let within = u64::from(sample) - base;
                cursor.chunk = first.wrapping_add((within / u64::from(spc)) as u32);
                index = (within % u64::from(spc)) as u32;
                cursor.chunk_left = spc - index;
                cursor.stsc_run = run;
                found = true;
                break;
            }
            base = base.wrapping_add(run_samples);
        }
        if !found {
            return cursor;
        }

        cursor.offset = self.chunk_offset(moov, cursor.chunk);
        for i in sample - index..sample {
            cursor.offset = cursor.offset.wrapping_add(u64::from(self.sample_size(moov, i)));
        }

        let Some((run, left, dts)) = find_run(moov, &self.stts, sample) else {
            return cursor;
        };
        cursor.stts_run = run;
        cursor.stts_left = left;
        cursor.dts = dts;
        if self.ctts.at.is_some() {
            if let Some((run, left, _)) = find_run(moov, &self.ctts, sample) {
                cursor.ctts_run = run;
                cursor.ctts_left = left;
            }
        }

        let (mut low, mut high) = (0, self.stss.count);
        while low < high {
            let middle = low + (high - low) / 2;
            if u64::from(self.stss.word(moov, middle, 4)) < u64::from(sample) + 1 {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        cursor.sync_next = low;
        cursor.valid = true;
        cursor
    }

    fn cursor_next(&self, moov: &[u8], cursor: &mut SampleCursor) {
        if !cursor.valid {
            return;
        }
        cursor.offset = cursor.offset.wrapping_add(u64::from(self.sample_size(moov, cursor.sample)));
        cursor.sample += 1;
        if cursor.sample >= self.sample_count {
            cursor.valid = false;
            return;
        }

        cursor.chunk_left -= 1;
        if cursor.chunk_left == 0 {
            cursor.chunk += 1;
            if cursor.chunk >= self.stco.count {
                cursor.valid = false;
                return;
            }
            if cursor.stsc_run + 1 < self.stsc.count && self.stsc_first(moov, cursor.stsc_run + 1).wrapping_sub(1) <= cursor.chunk {
                cursor.stsc_run += 1;
            }
            cursor.chunk_left = self.stsc_spc(moov, cursor.stsc_run);
            cursor.offset = self.chunk_offset(moov, cursor.chunk);
        }

        cursor.dts = cursor.dts.wrapping_add(u64::from(self.stts.run_value(moov, cursor.stts_run)));
        cursor.stts_left -= 1;
        if cursor.stts_left == 0 && !next_run(moov, &self.stts, &mut cursor.stts_run, &mut cursor.stts_left) {
            cursor.valid = false;
            return;
        }

        if cursor.ctts_left != 0 {
            cursor.ctts_left -= 1;
            if cursor.ctts_left == 0 && !next_run(moov, &self.ctts, &mut cursor.ctts_run, &mut cursor.ctts_left) {
                cursor.ctts_left = 0;
            }
        }

        while cursor.sync_next < self.stss.count && u64::from(self.stss.word(moov, cursor.sync_next, 4)) < u64::from(cursor.sample) + 1 {
            cursor.sync_next += 1;
        }
    }

    fn cursor_advance(&self, moov: &[u8], cursor: &mut SampleCursor, samples: u32) {
        for _ in 0..samples {
            if !cursor.valid {
                break;
            }
            self.cursor_next(moov, cursor);
        }
    }

    fn cursor_pts_us(&self, moov: &[u8], cursor: &SampleCursor) -> i64 {
        let offset = if cursor.ctts_left != 0 { i64::from(self.ctts.run_value(moov, cursor.ctts_run) as i32) } else { 0 };
        let time = (cursor.dts as i64).wrapping_add(offset).wrapping_sub(self.media_time);
        ts_to_us(time, self.timescale).wrapping_add(self.delay_us)
    }

    fn cursor_keyframe(&self, moov: &[u8], cursor: &SampleCursor) -> bool {
        if self.stss.at.is_none() {
            return true;
        }
        cursor.sync_next < self.stss.count && u64::from(self.stss.word(moov, cursor.sync_next, 4)) == u64::from(cursor.sample) + 1
    }

    fn sample_at_us(&self, moov: &[u8], us: i64, after: bool) -> u32 {
        let target = us_to_ts(us.wrapping_sub(self.delay_us), self.timescale).wrapping_add(self.media_time).max(0) as u64;

        let (mut start, mut time) = (0u64, 0u64);
        for run in 0..self.stts.count {
            let count = self.stts.run_count(moov, run);
            let delta = self.stts.run_value(moov, run);
            let length = u64::from(count) * u64::from(delta);
            if target < time.wrapping_add(length) {
                let within = target - time;
                let delta = u64::from(delta);
                let step = if after { within.div_ceil(delta) } else { within / delta }.min(u64::from(count));
                let sample = start + step;
                if sample >= u64::from(self.sample_count) {
                    break;
                }
                return sample as u32;
            }
            start += u64::from(count);
            time = time.wrapping_add(length);
        }
        if after { self.sample_count } else { self.sample_count - 1 }
    }

    fn sync_at_or_before(&self, moov: &[u8], sample: u32) -> u32 {
        if self.stss.at.is_none() {
            return sample;
        }
        let (mut low, mut high) = (0, self.stss.count);
        while low < high {
            let middle = low + (high - low) / 2;
            if u64::from(self.stss.word(moov, middle, 4)) <= u64::from(sample) + 1 {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low == 0 {
            return 0;
        }
        self.stss.word(moov, low - 1, 4).saturating_sub(1)
    }

    fn apply_edits(&mut self, moov: &[u8], movie_timescale: u32) {
        let Some(elst) = self.elst.filter(|elst| elst.len() >= 8) else {
            return;
        };
        let wide = moov[elst.start] == 1;
        let entry = if wide { 20 } else { 12 };
        let count = be32(moov, elst.start + 4);
        if count as usize > (elst.len() - 8) / entry {
            return;
        }

        let mut have_media = false;
        for i in 0..count as usize {
            let p = elst.start + 8 + i * entry;
            let duration = if wide { be64(moov, p) } else { u64::from(be32(moov, p)) };
            let media_time = if wide { be64(moov, p + 8) as i64 } else { i64::from(be32(moov, p + 4) as i32) };
            if media_time == -1 {
                if !have_media {
                    self.delay_us = self.delay_us.wrapping_add(ts_to_us(duration as i64, movie_timescale));
                }
                continue;
            }
            if have_media {
                warn!(TAG, "ignoring edits after the first");
                break;
            }
            self.media_time = media_time;
            have_media = true;
        }
    }

    fn first_entry(&self, moov: &[u8]) -> Option<(u32, Span)> {
        let stsd = self.stsd.filter(|stsd| stsd.len() >= 8)?;
        span_box(moov, &mut stsd.from(8))
    }

    fn identify_video(&mut self, moov: &[u8]) {
        self.codec = Some(Codec::Video(VideoCodec::Unsupported));
        let Some((kind, entry)) = self.first_entry(moov).filter(|(_, entry)| entry.len() >= VISUAL_ENTRY_BYTES) else {
            return;
        };
        self.width = be16(moov, entry.start + 24);
        self.height = be16(moov, entry.start + 26);
        let children = entry.from(VISUAL_ENTRY_BYTES);

        let codec = match kind {
            ENTRY_AVC1 | ENTRY_AVC3 => {
                self.codec_private = span_find(moov, children, BOX_AVCC);
                VideoCodec::H264
            }
            ENTRY_JPEG | ENTRY_MJPA => VideoCodec::Mjpeg,
            ENTRY_MP4V => {
                let config = span_find(moov, children, BOX_ESDS).and_then(|esds| parse_esds(moov, esds));
                match config {
                    Some(config) if config.object_type == OTI_MJPEG => VideoCodec::Mjpeg,
                    Some(config) if OTI_MPEG2_VIDEO.contains(&config.object_type) => {
                        self.codec_private = config.decoder_info;
                        VideoCodec::Mpeg2
                    }
                    _ => VideoCodec::Unsupported,
                }
            }
            ENTRY_M2V1 => VideoCodec::Mpeg2,
            _ => VideoCodec::Unsupported,
        };
        self.codec = Some(Codec::Video(codec));
    }

    fn identify_audio(&mut self, moov: &[u8]) {
        self.codec = Some(Codec::Audio(AudioCodec::Unsupported));
        let Some((kind, entry)) = self.first_entry(moov).filter(|(_, entry)| entry.len() >= AUDIO_ENTRY_BYTES) else {
            return;
        };

        let version = be16(moov, entry.start + 8);
        let mut header = AUDIO_ENTRY_BYTES;
        self.channels = be16(moov, entry.start + 16) as u8;
        self.bits = be16(moov, entry.start + 18) as u8;
        self.sample_rate = be32(moov, entry.start + 24) >> 16;
        if version == 1 {
            header = AUDIO_ENTRY_V1_BYTES;
        } else if version == 2 {
            header = AUDIO_ENTRY_V2_BYTES;
            if entry.len() < header {
                return;
            }
            self.sample_rate = math::lround(f64::from_bits(be64(moov, entry.start + 32))) as u32;
            self.channels = be32(moov, entry.start + 40) as u8;
            self.bits = be32(moov, entry.start + 48) as u8;
        }
        if entry.len() < header {
            return;
        }
        let children = entry.from(header);

        let mut codec = AudioCodec::Unsupported;
        match kind {
            ENTRY_MP4A => {
                if let Some(config) = find_in_wave(moov, children, BOX_ESDS).and_then(|esds| parse_esds(moov, esds)) {
                    self.bitrate_bps = config.avg_bitrate;
                    if config.object_type == OTI_MPEG4_AUDIO || OTI_MPEG2_AAC.contains(&config.object_type) {
                        codec = AudioCodec::Aac;
                        self.codec_private = config.decoder_info;
                    } else if config.object_type == OTI_MPEG2_MP3 || config.object_type == OTI_MPEG1_MP3 {
                        codec = AudioCodec::Mp3;
                    }
                }
            }
            ENTRY_MP3 => codec = AudioCodec::Mp3,
            ENTRY_OPUS => {
                if let Some(body) = span_find(moov, children, BOX_DOPS).filter(|body| body.len() >= DOPS_BYTES) {
                    codec = AudioCodec::Opus;
                    self.codec_private = Some(body);
                    self.channels = moov[body.start + 1];
                    self.opus = true;
                }
            }
            ENTRY_FLAC => {
                let blocks = span_find(moov, children, BOX_DFLA).filter(|body| body.len() >= 4);
                if let Some(si) = blocks.and_then(|body| flac::find_streaminfo(body.from(4).bytes(moov))) {
                    codec = AudioCodec::Flac;
                    self.codec_private = Some(inner_span(moov, si));
                    (self.sample_rate, self.channels, self.bits) = flac::streaminfo_format(si);
                }
            }
            ENTRY_ALAC => {
                if let Some(cookie) = find_in_wave(moov, children, BOX_ALAC).and_then(|body| alac::find_cookie(body.bytes(moov))) {
                    codec = AudioCodec::Alac;
                    self.codec_private = Some(inner_span(moov, cookie));
                    (self.sample_rate, self.channels, self.bits) = alac::cookie_format(cookie);
                }
            }
            ENTRY_SOWT => {
                if self.bits == 16 && self.channels != 0 {
                    codec = AudioCodec::Pcm;
                    self.fixed_size = u32::from(self.channels) * 2;
                }
            }
            _ => {}
        }
        if self.sample_rate == 0 || self.channels == 0 {
            codec = AudioCodec::Unsupported;
        }
        self.codec = Some(Codec::Audio(codec));
    }

    fn score(&self, moov: &[u8]) -> i32 {
        if !self.tables_valid(moov) {
            return -1;
        }
        let supported = !matches!(self.codec, Some(Codec::Video(VideoCodec::Unsupported) | Codec::Audio(AudioCodec::Unsupported)));
        (if supported { 2 } else { 0 }) + i32::from(self.enabled)
    }

    fn rotation(&self) -> u16 {
        if self.matrix_a == 0 && self.matrix_b == 0 {
            return 0;
        }
        let degrees = -math::atan2(f64::from(self.matrix_b), f64::from(self.matrix_a)) * 180.0 / PI;
        let quarters = math::round(degrees / 90.0);
        if (degrees - quarters * 90.0).abs() > ROTATION_TOLERANCE_DEG {
            warn!(TAG, "ignoring rotation {degrees:.1}, not a multiple of 90");
            return 0;
        }
        ((quarters as i32).rem_euclid(4) * 90) as u16
    }

    fn frame_interval_us(&self, moov: &[u8], mjpeg: bool) -> i64 {
        let mut best = 0;
        for run in 1..self.stts.count {
            let better = if mjpeg {
                // A still picture is stored once with a long duration, so the
                // most common delta can be the length of a still.
                let value = self.stts.run_value(moov, run);
                let shortest = self.stts.run_value(moov, best);
                value != 0 && (shortest == 0 || value < shortest)
            } else {
                self.stts.run_count(moov, run) > self.stts.run_count(moov, best)
            };
            if better {
                best = run;
            }
        }
        ts_to_us(i64::from(self.stts.run_value(moov, best)), self.timescale)
    }
}

fn find_run(moov: &[u8], table: &Table, sample: u32) -> Option<(u32, u32, u64)> {
    let (mut start, mut total) = (0u64, 0u64);
    for i in 0..table.count {
        let count = u64::from(table.run_count(moov, i));
        let value = u64::from(table.run_value(moov, i));
        if u64::from(sample) < start + count {
            let left = (start + count - u64::from(sample)) as u32;
            return Some((i, left, total.wrapping_add((u64::from(sample) - start).wrapping_mul(value))));
        }
        start += count;
        total = total.wrapping_add(count.wrapping_mul(value));
    }
    None
}

fn next_run(moov: &[u8], table: &Table, run: &mut u32, left: &mut u32) -> bool {
    loop {
        *run += 1;
        if *run >= table.count {
            return false;
        }
        let count = table.run_count(moov, *run);
        if count != 0 {
            *left = count;
            return true;
        }
    }
}

fn read_descriptor(moov: &[u8], span: &mut Span) -> Option<(u8, Span)> {
    if span.start >= span.end {
        return None;
    }
    let tag = moov[span.start];
    span.start += 1;
    let mut length = 0u32;
    for _ in 0..4 {
        if span.start >= span.end {
            return None;
        }
        let byte = moov[span.start];
        span.start += 1;
        length = length << 7 | u32::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            break;
        }
    }
    if length as usize > span.len() {
        return None;
    }
    let body = Span { start: span.start, end: span.start + length as usize };
    span.start = body.end;
    Some((tag, body))
}

struct DecoderConfig {
    object_type: u8,
    avg_bitrate: u32,
    decoder_info: Option<Span>,
}

fn parse_esds(moov: &[u8], esds: Span) -> Option<DecoderConfig> {
    if esds.len() < 4 {
        return None;
    }
    let mut rest = esds.from(4);
    let (tag, mut es) = read_descriptor(moov, &mut rest)?;
    if tag != DESC_ES || es.len() < 3 {
        return None;
    }
    let flags = moov[es.start + 2];
    let mut at = es.start + 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        if at >= es.end {
            return None;
        }
        at += 1 + usize::from(moov[at]);
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    if at > es.end {
        return None;
    }
    es.start = at;

    while let Some((tag, mut config)) = read_descriptor(moov, &mut es) {
        if tag != DESC_DECODER_CONFIG {
            continue;
        }
        if config.len() < DECODER_CONFIG_BYTES {
            return None;
        }
        let object_type = moov[config.start];
        let avg_bitrate = be32(moov, config.start + 9);
        config.start += DECODER_CONFIG_BYTES;
        let mut decoder_info = None;
        while let Some((tag, info)) = read_descriptor(moov, &mut config) {
            if tag == DESC_DECODER_INFO {
                decoder_info = Some(info);
                break;
            }
        }
        return Some(DecoderConfig { object_type, avg_bitrate, decoder_info });
    }
    None
}

fn find_in_wave(moov: &[u8], children: Span, kind: u32) -> Option<Span> {
    span_find(moov, children, kind).or_else(|| span_find(moov, span_find(moov, children, BOX_WAVE)?, kind))
}

fn inner_span(moov: &[u8], inner: &[u8]) -> Span {
    let start = inner.as_ptr() as usize - moov.as_ptr() as usize;
    Span { start, end: start + inner.len() }
}

fn tag_field_of(kind: u32) -> Option<Field> {
    match kind {
        BOX_NAM => Some(Field::Title),
        BOX_ART => Some(Field::Artist),
        BOX_ALB => Some(Field::Album),
        BOX_AART => Some(Field::AlbumArtist),
        BOX_DAY => Some(Field::Date),
        _ => None,
    }
}

fn parse_ilst(moov: &[u8], mut ilst: Span, tags: &mut Tags, want_cover: bool) {
    while let Some((kind, item)) = span_box(moov, &mut ilst) {
        let Some(data) = span_find(moov, item, BOX_DATA).filter(|data| data.len() >= 8) else {
            continue;
        };
        let class = be32(moov, data.start) & 0xFF_FFFF;
        let payload = data.from(8).bytes(moov);

        if let Some(field) = tag_field_of(kind) {
            tags.set(field, payload, if class == 2 { Encoding::Utf16Be } else { Encoding::Utf8 });
        } else if kind == BOX_TRKN && payload.len() >= 6 {
            tags.set_number(Field::Track, be16(payload, 2), be16(payload, 4));
        } else if kind == BOX_COVR && want_cover {
            tags.set_cover(payload, true);
        }
    }
}

// QuickTime writes meta as a plain box, ISO-BMFF as a full box.
fn parse_udta(moov: &[u8], udta: Span, tags: &mut Tags, want_cover: bool) {
    let Some(meta) = span_find(moov, udta, BOX_META) else {
        return;
    };
    let ilst = if meta.len() >= 4 { span_find(moov, meta.from(4), BOX_ILST) } else { None };
    if let Some(ilst) = ilst.or_else(|| span_find(moov, meta, BOX_ILST)) {
        parse_ilst(moov, ilst, tags, want_cover);
    }
}

fn printable(kind: u32) -> bool {
    kind.to_be_bytes().iter().all(|c| (0x20..=0x7E).contains(c))
}

fn keep_opus_head(p: &[u8]) -> Box<[u8]> {
    let table = &p[DOPS_BYTES..];
    let mut head = Vec::new();
    if head.try_reserve_exact(OPUS_HEAD_BYTES + table.len()).is_ok() {
        head.extend_from_slice(b"OpusHead");
        head.extend_from_slice(&[1, p[1], p[3], p[2], p[7], p[6], p[5], p[4], p[9], p[8], p[10]]);
        head.extend_from_slice(table);
    }
    head.into_boxed_slice()
}

fn copy_span(moov: &[u8], span: Option<Span>) -> Box<[u8]> {
    let mut copy = Vec::new();
    if let Some(span) = span {
        if copy.try_reserve_exact(span.len()).is_ok() {
            copy.extend_from_slice(span.bytes(moov));
        }
    }
    copy.into_boxed_slice()
}

/// ISO BMFF and QuickTime. Packets come out in timestamp order, not file order.
pub struct Mp4Demuxer {
    reader: Buffer,
    info: Info,
    max_view: usize,
    moov: Aligned,
    movie_timescale: u32,
    movie_duration: u64,
    video: Option<Track>,
    audio: Option<Track>,
    video_cursor: SampleCursor,
    audio_cursor: SampleCursor,
    want_audio: bool,
    warned_interleave: bool,
}

fn load_moov(reader: &Buffer) -> Result<Aligned> {
    let file_end = reader.size();
    let mut position = 0;
    let mut first = true;
    while position + 8 <= file_end {
        let mut header = [0; 16];
        reader.seek(position);
        if !reader.read_all(&mut header[..8]) {
            break;
        }
        let mut size = u64::from(be32(&header, 0));
        let kind = be32(&header, 4);
        let mut header_bytes = 8;
        if size == 1 {
            if !reader.read_all(&mut header[8..]) {
                break;
            }
            size = be64(&header, 8);
            header_bytes = 16;
        } else if size == 0 {
            size = (file_end - position) as u64;
        }
        if first && (!printable(kind) || size < header_bytes) {
            return Err(Error::Format("not an MP4 file"));
        }
        if size < header_bytes || size > (file_end - position) as u64 {
            break;
        }

        if kind == BOX_MOOF {
            return Err(Error::Format("fragmented MP4 is not supported"));
        }
        if kind == BOX_MOOV {
            let bytes = size - header_bytes;
            if bytes > MAX_MOOV_BYTES {
                return Err(Error::Format("MP4 index is too large"));
            }
            let mut moov = Aligned::new(0, bytes as usize).map_err(|_| Error::Format("MP4 index is too large"))?;
            if !reader.read_all(&mut moov) {
                return Err(Error::Format("cannot read the MP4 index"));
            }
            return Ok(moov);
        }
        position += size as i64;
        first = false;
    }
    Err(Error::Format(if first { "not an MP4 file" } else { "no moov in this MP4" }))
}

impl Mp4Demuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let max_view = reader.max_view();
        let moov = load_moov(&reader)?;
        let mut demux = Self {
            reader,
            info: Info::new(Container::Mp4),
            max_view,
            moov,
            movie_timescale: 0,
            movie_duration: 0,
            video: None,
            audio: None,
            video_cursor: SampleCursor::default(),
            audio_cursor: SampleCursor::default(),
            want_audio: true,
            warned_interleave: false,
        };
        demux.info.tags.cover_scanned = options.want_cover;

        demux.parse_moov(options.want_cover)?;
        demux.fill_info();
        if let Some(video) = &demux.video {
            demux.video_cursor = video.cursor_seek(&demux.moov, 0);
        }
        if let Some(audio) = &demux.audio {
            demux.audio_cursor = audio.cursor_seek(&demux.moov, 0);
        }

        info!(TAG, "{}: {} us, {} video samples, {} audio samples", path.to_str().unwrap_or("?"), demux.info.duration_us,
              demux.video.as_ref().map_or(0, |track| track.sample_count), demux.audio.as_ref().map_or(0, |track| track.sample_count));

        let start = if demux.video.is_some() { demux.video_cursor.offset } else { demux.audio_cursor.offset };
        demux.reader.seek(start as i64);
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn parse_moov(&mut self, want_cover: bool) -> Result<()> {
        let moov = &self.moov[..];
        let mut span = Span { start: 0, end: moov.len() };
        let (mut video_score, mut audio_score) = (-1, -1);
        let mut fragmented = false;
        while let Some((kind, body)) = span_box(moov, &mut span) {
            match kind {
                BOX_MVEX => fragmented = true,
                BOX_UDTA => parse_udta(moov, body, &mut self.info.tags, want_cover),
                BOX_MVHD if body.len() >= 4 => {
                    let mut header = Track::default();
                    header.parse_mdhd(moov, body);
                    self.movie_timescale = header.timescale;
                    self.movie_duration = header.duration;
                }
                BOX_TRAK => {
                    let mut track = Track::parse(moov, body);
                    if track.handler == HANDLER_VIDEO {
                        track.identify_video(moov);
                        let score = track.score(moov);
                        if score > video_score {
                            video_score = score;
                            self.video = Some(track);
                        }
                    } else if track.handler == HANDLER_AUDIO {
                        track.identify_audio(moov);
                        let score = track.score(moov);
                        if score > audio_score {
                            audio_score = score;
                            self.audio = Some(track);
                        }
                    }
                }
                _ => {}
            }
        }
        if fragmented {
            return Err(Error::Format("fragmented MP4 is not supported"));
        }
        match (&self.video, &self.audio) {
            (None, None) => Err(Error::Format("no playable track")),
            (Some(video), _) if matches!(video.codec, Some(Codec::Video(VideoCodec::Unsupported))) => {
                Err(Error::Format("unsupported video codec in this MP4"))
            }
            (None, Some(audio)) if matches!(audio.codec, Some(Codec::Audio(AudioCodec::Unsupported))) => {
                Err(Error::Format("unsupported audio codec in this MP4"))
            }
            _ => Ok(()),
        }
    }

    fn fill_info(&mut self) {
        let moov = &self.moov[..];
        if let Some(track) = self.video.as_mut() {
            track.apply_edits(moov, self.movie_timescale);
            let codec = match track.codec {
                Some(Codec::Video(codec)) => codec,
                _ => VideoCodec::Unsupported,
            };
            self.info.video = Some(VideoTrack {
                codec,
                width: track.width,
                height: track.height,
                frame_interval_us: track.frame_interval_us(moov, codec == VideoCodec::Mjpeg),
                rotation_ccw: track.rotation(),
                frame_count: 0,
                max_frame_bytes: 0,
                codec_private: copy_span(moov, track.codec_private),
            });
        }

        if let Some(track) = self.audio.as_mut() {
            track.apply_edits(moov, self.movie_timescale);
            let codec = match track.codec {
                Some(Codec::Audio(codec)) => codec,
                _ => AudioCodec::Unsupported,
            };
            let codec_private = match track.codec_private {
                Some(body) if track.opus => keep_opus_head(body.bytes(moov)),
                span => copy_span(moov, span),
            };
            self.info.audio = Some(AudioTrack {
                codec,
                sample_rate: track.sample_rate,
                bitrate_bps: track.bitrate_bps,
                channels: track.channels,
                bits_per_sample: if track.bits != 0 { track.bits } else { 16 },
                block_align: 0,
                max_frame_bytes: 0,
                codec_private,
            });
        }

        let mut duration = 0;
        if self.movie_timescale != 0
            && self.movie_duration != 0
            && self.movie_duration != u64::from(u32::MAX)
            && self.movie_duration != u64::MAX
        {
            duration = ts_to_us(self.movie_duration as i64, self.movie_timescale);
        }
        if duration <= 0 {
            if let Some(timed) = self.video.as_ref().or(self.audio.as_ref()) {
                duration = ts_to_us(timed.duration as i64, timed.timescale);
            }
        }
        self.info.duration_us = duration;
    }

    fn warn_interleave(&mut self) {
        if self.warned_interleave {
            return;
        }
        let distance = self.video_cursor.offset.abs_diff(self.audio_cursor.offset);
        if distance > self.reader.arena_bytes() as u64 * 3 / 8 {
            warn!(TAG, "audio and video are {distance} bytes apart; reading will be slow");
            self.warned_interleave = true;
        }
    }

    fn video_track(&self) -> Option<(&Track, &[u8])> {
        self.video.as_ref().map(|track| (track, &self.moov[..]))
    }

    fn keyframe_cursor(&self, pts_us: i64) -> Option<(&Track, SampleCursor)> {
        let (video, moov) = self.video_track()?;
        let cursor = video.cursor_seek(moov, video.sync_at_or_before(moov, video.sample_at_us(moov, pts_us, false)));
        cursor.valid.then_some((video, cursor))
    }

    /// Where the sync sample at or before `pts_us` sits in the file, as offset
    /// and size.
    pub fn keyframe_at(&self, pts_us: i64) -> Option<(u64, u32)> {
        let (video, cursor) = self.keyframe_cursor(pts_us)?;
        let bytes = video.sample_size(&self.moov, cursor.sample);
        if bytes == 0 || cursor.offset.wrapping_add(u64::from(bytes)) > self.reader.size() as u64 {
            return None;
        }
        Some((cursor.offset, bytes))
    }

    pub fn keyframe_before(&self, pts_us: i64) -> Option<i64> {
        if self.info.video.as_ref()?.codec == VideoCodec::Mjpeg {
            return None;
        }
        let (video, cursor) = self.keyframe_cursor(pts_us)?;
        Some(video.cursor_pts_us(&self.moov, &cursor))
    }
}

impl Demuxer for Mp4Demuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        let file_size = self.reader.size() as u64;
        let audio_usable = self.info.audio_usable();
        loop {
            let moov = &self.moov[..];
            let audio = self.want_audio && audio_usable && self.audio_cursor.valid;
            let video = self.video_cursor.valid;
            if !video && !audio {
                return Ok(None);
            }

            let video_pts = match &self.video {
                Some(track) if video => track.cursor_pts_us(moov, &self.video_cursor),
                _ => 0,
            };
            let audio_pts = match &self.audio {
                Some(track) if audio => track.cursor_pts_us(moov, &self.audio_cursor),
                _ => 0,
            };
            let is_video = video && (!audio || video_pts <= audio_pts);
            let (track, cursor) = if is_video {
                (self.video.as_ref(), &mut self.video_cursor)
            } else {
                (self.audio.as_ref(), &mut self.audio_cursor)
            };
            let Some(track) = track else {
                return Ok(None);
            };

            let mut samples = 1;
            let mut size = u64::from(track.sample_size(moov, cursor.sample));
            if !is_video && track.fixed_size != 0 {
                samples = cursor.chunk_left.min(PCM_PACKET_BYTES / track.fixed_size).min(track.sample_count - cursor.sample);
                size = u64::from(samples) * u64::from(track.fixed_size);
            }
            let offset = cursor.offset;
            let pts = if is_video { video_pts } else { audio_pts };

            if size == 0 || size > self.max_view as u64 || offset.saturating_add(size) > file_size {
                track.cursor_advance(moov, cursor, samples);
                continue;
            }

            let mut floor = offset;
            if video {
                floor = floor.min(self.video_cursor.offset);
            }
            if audio {
                floor = floor.min(self.audio_cursor.offset);
            }
            if floor as i64 > self.reader.tell() {
                self.reader.seek(floor as i64);
            }
            if video && audio {
                self.warn_interleave();
            }

            let data = self.reader.view_at(offset as i64, size as usize)?;

            let moov = &self.moov[..];
            let (track, cursor) = if is_video {
                (self.video.as_ref(), &mut self.video_cursor)
            } else {
                (self.audio.as_ref(), &mut self.audio_cursor)
            };
            let Some(track) = track else {
                return Ok(None);
            };
            let keyframe = !is_video || track.cursor_keyframe(moov, cursor);
            track.cursor_advance(moov, cursor, samples);
            let kind = if is_video { PacketKind::Video } else { PacketKind::Audio };
            return Ok(Some(Packet { kind, pts_us: pts, keyframe, data, frame: 0 }));
        }
    }

    /// H.264 lands on the last sync sample at or before the target and reports
    /// its time; MJPEG lands on the sample on screen and reports the target.
    /// Audio starts at its first sample at or after where the video landed.
    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        let moov = &self.moov[..];
        let Some(video) = &self.video else {
            let audio = self.audio.as_ref().ok_or(Error::NotSeekable)?;
            let sample = if pts_us > 0 { audio.sample_at_us(moov, pts_us, false) } else { 0 };
            self.audio_cursor = audio.cursor_seek(moov, sample);
            if !self.audio_cursor.valid {
                return Err(Error::NotSeekable);
            }
            return Ok(audio.cursor_pts_us(moov, &self.audio_cursor).max(0));
        };

        let mjpeg = self.info.video.as_ref().is_some_and(|track| track.codec == VideoCodec::Mjpeg);
        let mut landed = 0;
        if pts_us <= 0 {
            self.video_cursor = video.cursor_seek(moov, 0);
        } else if !mjpeg {
            self.video_cursor = video.cursor_seek(moov, video.sync_at_or_before(moov, video.sample_at_us(moov, pts_us, false)));
            if !self.video_cursor.valid {
                return Err(Error::NotSeekable);
            }
            landed = video.cursor_pts_us(moov, &self.video_cursor).max(0);
        } else {
            self.video_cursor = video.cursor_seek(moov, video.sample_at_us(moov, pts_us, false));
            if !self.video_cursor.valid {
                return Err(Error::NotSeekable);
            }
            landed = pts_us;
        }

        if let Some(audio) = &self.audio {
            let sample = if landed > 0 { audio.sample_at_us(moov, landed, true) } else { 0 };
            self.audio_cursor = audio.cursor_seek(moov, sample);
        }
        Ok(landed)
    }

    fn set_want_audio(&mut self, want: bool) {
        self.want_audio = want;
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}
