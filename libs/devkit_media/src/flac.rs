/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::CStr;

use idf::heap::{self, Caps};
use idf::log::{info, warn};

use crate::buffer::{Arena, Buffer, psram_first};
use crate::demux::{AudioCodec, AudioTrack, Container, Demuxer, Info, OpenOptions, Packet};
use crate::error::{Error, Result};
use crate::id3;
use crate::tags::Cover;
use crate::vorbis;

const TAG: &CStr = c"media_flac";

pub const STREAMINFO_BYTES: usize = 34;

const HEADER_MAX_BYTES: usize = 16;
const SCAN_BYTES: usize = 4096;
const WINDOW_BYTES: usize = 16 * 1024;
const MAX_SEEK_POINTS: u32 = 4096;
const SEEK_POINT_BYTES: usize = 18;
const SEEK_STEPS: usize = 32;

const BLOCK_STREAMINFO: u8 = 0;
const BLOCK_SEEKTABLE: u8 = 3;
const BLOCK_VORBIS_COMMENT: u8 = 4;
const BLOCK_PICTURE: u8 = 6;

const RATES: [u32; 12] = [0, 88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000];
const SIZES: [u8; 8] = [0, 8, 12, 0, 16, 20, 24, 32];

#[derive(Clone, Copy, Default)]
struct Header {
    sample: u64,
    block: u32,
    length: u8,
}

#[derive(Clone, Copy, Default)]
struct Frame {
    offset: i64,
    h: Header,
}

#[derive(Clone, Copy)]
struct SeekPoint {
    sample: u64,
    offset: u64,
}

fn be16(p: &[u8]) -> u32 {
    u32::from(p[0]) << 8 | u32::from(p[1])
}

fn be24(p: &[u8]) -> u32 {
    u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])
}

fn be64(p: &[u8]) -> u64 {
    u64::from_be_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]])
}

fn crc8(p: &[u8]) -> u8 {
    p.iter().fold(0, |mut crc, &byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { crc << 1 ^ 0x07 } else { crc << 1 };
        }
        crc
    })
}

/// The STREAMINFO body in a list of metadata blocks, with or without "fLaC" in
/// front (Matroska CodecPrivate, the blocks of an MP4 dfLa).
pub fn find_streaminfo(blocks: &[u8]) -> Option<&[u8]> {
    let size = blocks.len();
    let mut pos = if blocks.starts_with(b"fLaC") { 4 } else { 0 };
    while pos + 4 <= size {
        let kind = blocks[pos] & 0x7F;
        let len = be24(&blocks[pos + 1..]) as usize;
        if len > size - pos - 4 {
            return None;
        }
        if kind == BLOCK_STREAMINFO && len >= STREAMINFO_BYTES {
            return Some(&blocks[pos + 4..pos + 4 + STREAMINFO_BYTES]);
        }
        if blocks[pos] & 0x80 != 0 {
            return None;
        }
        pos += 4 + len;
    }
    None
}

/// Sample rate, channels and bits per sample.
pub fn streaminfo_format(si: &[u8]) -> (u32, u8, u8) {
    let rate = u32::from(si[10]) << 12 | u32::from(si[11]) << 4 | u32::from(si[12]) >> 4;
    let channels = ((si[12] >> 1) & 7) + 1;
    let bits = (((si[12] & 1) << 4) | si[13] >> 4) + 1;
    (rate, channels, bits)
}

pub struct FlacDemuxer {
    reader: Buffer,
    info: Info,
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    data_start: i64,
    data_end: i64,
    cursor: i64,
    max_view: usize,
    total_samples: u64,
    max_block: u32,
    max_frame: u32,
    nominal_block: u32,
    variable: bool,
    strategy_known: bool,
    sample_numbered: bool,
    points: Vec<SeekPoint>,
    scan: Box<[u8; SCAN_BYTES], Caps>,
}

impl FlacDemuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let max_view = reader.max_view();
        let scan = psram_first(heap::zeroed_box)?;
        let mut demux = Self {
            reader,
            info: Info::new(Container::Flac),
            sample_rate: 0,
            channels: 0,
            bits_per_sample: 0,
            data_start: 0,
            data_end: 0,
            cursor: 0,
            max_view,
            total_samples: 0,
            max_block: 0,
            max_frame: 0,
            nominal_block: 0,
            variable: false,
            strategy_known: false,
            sample_numbered: false,
            points: Vec::new(),
            scan,
        };
        demux.info.tags.cover_scanned = true;

        let start = id3::read_v2(&demux.reader, 0, &mut demux.info.tags, options.want_cover).unwrap_or(0);
        let mut magic = [0; 4];
        if !demux.reader.read_exact_at(start, &mut magic) || magic != *b"fLaC" {
            return Err(Error::Format("not a FLAC file"));
        }
        let streaminfo = demux.read_blocks(start + 4, options.want_cover)?;
        let (sample_rate, channels, bits) = streaminfo_format(&streaminfo);
        demux.sample_rate = sample_rate;
        demux.channels = channels;
        demux.bits_per_sample = bits;
        demux.max_block = be16(&streaminfo[2..]);
        demux.max_frame = be24(&streaminfo[7..]);
        demux.total_samples = u64::from(streaminfo[13] & 0x0F) << 32
            | u64::from(u32::from_be_bytes([streaminfo[14], streaminfo[15], streaminfo[16], streaminfo[17]]));
        if sample_rate == 0 || demux.max_block == 0 {
            return Err(Error::Format("unusable FLAC STREAMINFO"));
        }

        demux.data_end = demux.reader.size();
        if demux.data_end - demux.data_start >= 128 {
            let mut tail = [0; 3];
            if demux.reader.read_exact_at(demux.data_end - 128, &mut tail) && tail == *b"TAG" {
                id3::read_v1(&demux.reader, demux.data_end, &mut demux.info.tags);
                demux.data_end -= 128;
            }
        }
        if !demux.find_first_frame() {
            return Err(Error::Format("no FLAC frames in this file"));
        }
        for point in &mut demux.points {
            point.offset = point.offset.wrapping_add(demux.data_start as u64);
        }

        if demux.total_samples == 0 {
            demux.total_samples = demux.count_samples();
        }
        demux.info.duration_us = (demux.total_samples.wrapping_mul(1_000_000) / u64::from(sample_rate)) as i64;
        let bitrate_bps = if demux.info.duration_us > 0 {
            ((demux.data_end - demux.data_start) as u64 * 8 * 1_000_000 / demux.info.duration_us as u64) as u32
        } else {
            0
        };
        let mut codec_private = Vec::new();
        codec_private.extend_from_slice(&streaminfo);
        demux.info.audio = Some(AudioTrack {
            codec: AudioCodec::Flac,
            sample_rate,
            bitrate_bps,
            channels,
            bits_per_sample: bits,
            block_align: 0,
            max_frame_bytes: 0,
            codec_private: codec_private.into_boxed_slice(),
        });

        info!(TAG, "{}: {sample_rate} Hz x{channels}, {bits} bit, {} us, {} seek points",
              path.to_str().unwrap_or("?"), demux.info.duration_us, demux.points.len());

        demux.cursor = demux.data_start;
        demux.reader.seek(demux.cursor);
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn read_blocks(&mut self, mut pos: i64, want_cover: bool) -> Result<[u8; STREAMINFO_BYTES]> {
        let mut streaminfo = None;
        let mut have_front = false;
        let size = self.reader.size();
        let mut last = false;
        while !last {
            let mut head = [0; 4];
            if !self.reader.read_exact_at(pos, &mut head) {
                return Err(Error::Format("cannot read the FLAC header"));
            }
            last = head[0] & 0x80 != 0;
            let kind = head[0] & 0x7F;
            let len = be24(&head[1..]);
            let body = pos + 4;
            if body + i64::from(len) > size {
                return Err(Error::Format("broken FLAC metadata"));
            }
            if kind == BLOCK_STREAMINFO && len as usize >= STREAMINFO_BYTES {
                let mut bytes = [0; STREAMINFO_BYTES];
                streaminfo = self.reader.read_exact_at(body, &mut bytes).then_some(bytes);
            } else if kind == BLOCK_SEEKTABLE && self.points.is_empty() && len as usize >= SEEK_POINT_BYTES {
                self.read_seek_table(body, (len / SEEK_POINT_BYTES as u32).min(MAX_SEEK_POINTS));
            } else if kind == BLOCK_VORBIS_COMMENT {
                vorbis::read_comment(&self.reader, body, len, &mut self.info.tags);
            } else if kind == BLOCK_PICTURE {
                vorbis::read_flac_picture(&self.reader, body, len, &mut self.info.tags, want_cover, &mut have_front);
            }
            pos = body + i64::from(len);
        }
        self.data_start = pos;
        streaminfo.ok_or(Error::Format("no STREAMINFO in this FLAC file"))
    }

    fn read_seek_table(&mut self, body: i64, count: u32) {
        if self.points.try_reserve_exact(count as usize).is_err() {
            return;
        }
        for i in 0..count {
            let mut point = [0; SEEK_POINT_BYTES];
            if !self.reader.read_exact_at(body + i64::from(i) * SEEK_POINT_BYTES as i64, &mut point) {
                break;
            }
            let sample = be64(&point);
            if sample != u64::MAX {
                self.points.push(SeekPoint { sample, offset: be64(&point[8..]) });
            }
        }
    }

    // A frame header that agrees with STREAMINFO and its own CRC-8. Sync words
    // turn up in compressed data, so nothing less is trusted.
    fn parse_header(&self, p: &[u8]) -> Option<Header> {
        let avail = p.len();
        if avail < 6 || p[0] != 0xFF || p[1] & 0xFE != 0xF8 {
            return None;
        }
        let variable = p[1] & 1 != 0;
        if self.strategy_known && variable != self.variable {
            return None;
        }
        let block_code = p[2] >> 4;
        let rate_code = p[2] & 0x0F;
        let assignment = p[3] >> 4;
        let size_code = usize::from((p[3] >> 1) & 7);
        if block_code == 0 || rate_code == 15 || assignment > 10 || p[3] & 1 != 0 {
            return None;
        }
        if (if assignment < 8 { assignment + 1 } else { 2 }) != self.channels {
            return None;
        }
        if size_code == 3 || (size_code != 0 && SIZES[size_code] != self.bits_per_sample) {
            return None;
        }

        let ones = p[4].leading_ones() as usize;
        if ones == 1 || ones == 8 {
            return None;
        }
        let coded = ones.max(1);
        if avail < 4 + coded + 5 {
            return None;
        }
        let mut number = u64::from(p[4] & (0x7F >> ones));
        for &byte in &p[5..4 + coded] {
            if byte & 0xC0 != 0x80 {
                return None;
            }
            number = number << 6 | u64::from(byte & 0x3F);
        }
        let mut i = 4 + coded;

        let block = match block_code {
            1 => 192,
            2..=5 => 576 << (block_code - 2),
            6 => {
                i += 1;
                u32::from(p[i - 1]) + 1
            }
            7 => {
                i += 2;
                be16(&p[i - 2..]) + 1
            }
            _ => 256 << (block_code - 8),
        };

        let mut rate = self.sample_rate;
        if (1..=11).contains(&rate_code) {
            rate = RATES[usize::from(rate_code)];
        }
        if rate_code == 12 {
            rate = u32::from(p[i]) * 1000;
            i += 1;
        }
        if rate_code == 13 || rate_code == 14 {
            rate = be16(&p[i..]) * if rate_code == 14 { 10 } else { 1 };
            i += 2;
        }
        if rate != self.sample_rate || block > self.max_block {
            return None;
        }
        if i >= avail || crc8(&p[..i]) != p[i] {
            return None;
        }

        let sample = if variable || self.sample_numbered { number } else { number.wrapping_mul(u64::from(self.nominal_block)) };
        Some(Header { sample, block, length: (i + 1) as u8 })
    }

    // First header in p[from..usable] that parses (and, with `expect`, starts
    // at that sample); bytes up to the end of `p` may complete it.
    fn scan_span(&self, p: &[u8], from: usize, usable: usize, expect: Option<u64>) -> Option<(usize, Header)> {
        let mut i = from;
        while let Some(skip) = p.get(i..usable)?.iter().position(|&b| b == 0xFF) {
            i += skip;
            if let Some(header) = self.parse_header(&p[i..]) {
                if expect.is_none() || expect == Some(header.sample) {
                    return Some((i, header));
                }
            }
            i += 1;
        }
        None
    }

    fn find_header(&mut self, from: i64, limit: i64, expect: Option<u64>) -> Option<Frame> {
        let limit = limit.min(self.data_end);
        let mut at = from;
        while at < limit {
            let want = (self.data_end - at).min(SCAN_BYTES as i64) as usize;
            self.reader.seek(at);
            let got = self.reader.read(&mut self.scan[..want]);
            if got < 2 {
                return None;
            }
            let last = at + got as i64 >= self.data_end;
            let mut usable = if last { got } else { got - (HEADER_MAX_BYTES - 1) };
            if at + usable as i64 > limit {
                usable = (limit - at) as usize;
            }
            if let Some((i, h)) = self.scan_span(&self.scan[..got], 0, usable, expect) {
                return Some(Frame { offset: at + i as i64, h });
            }
            if last {
                return None;
            }
            at += usable as i64;
        }
        None
    }

    // No frame is larger than its samples stored verbatim, which holds even
    // where STREAMINFO's maximum frame size is wrong.
    fn frame_reach(&self) -> i64 {
        let verbatim = u64::from(self.max_block) * u64::from(self.channels) * (u64::from(self.bits_per_sample) + 1) / 8
            + 2 * HEADER_MAX_BYTES as u64;
        verbatim.min(self.max_view as u64) as i64
    }

    // The frame after `f`, found by the sample it has to start at. `Err(at_end)`
    // at the end of the data, and when nothing follows within reach.
    fn next_frame(&mut self, f: &Frame) -> core::result::Result<Frame, bool> {
        let expect = f.h.sample.wrapping_add(u64::from(f.h.block));
        let limit = f.offset + self.frame_reach();
        self.find_header(f.offset + i64::from(f.h.length), limit, Some(expect)).ok_or(limit >= self.data_end)
    }

    fn frame_at_or_after(&mut self, mut from: i64, limit: i64) -> Option<Frame> {
        while from < limit {
            let found = self.find_header(from, limit, None)?;
            if matches!(self.next_frame(&found), Ok(_) | Err(true)) {
                return Some(found);
            }
            from = found.offset + 1;
        }
        None
    }

    fn find_first_frame(&mut self) -> bool {
        let mut head = [0; HEADER_MAX_BYTES];
        let want = (self.data_end - self.data_start).min(HEADER_MAX_BYTES as i64).max(0) as usize;
        if want < 2 || !self.reader.read_exact_at(self.data_start, &mut head[..want]) {
            return false;
        }
        self.variable = head[1] & 1 != 0;
        self.strategy_known = true;
        self.nominal_block = 0;
        let mut first = match self.parse_header(&head[..want]) {
            Some(h) => Frame { offset: self.data_start, h },
            None => {
                let limit = self.data_start + self.frame_reach();
                match self.find_header(self.data_start + 1, limit, None) {
                    Some(frame) => frame,
                    None => return false,
                }
            }
        };
        self.nominal_block = first.h.block;
        first.h.sample = 0;
        self.data_start = first.offset;

        // Old encoders wrote sample numbers under the fixed-size flag; the
        // second frame's number tells the two apart.
        if !self.variable && matches!(self.next_frame(&first), Err(false)) {
            self.sample_numbered = true;
            if self.next_frame(&first).is_err() {
                self.sample_numbered = false;
            }
        }
        true
    }

    fn count_samples(&mut self) -> u64 {
        let from = (self.data_end - 2 * self.frame_reach()).max(self.data_start);
        let Some(mut f) = self.frame_at_or_after(from, self.data_end) else {
            return 0;
        };
        while let Ok(next) = self.next_frame(&f) {
            f = next;
        }
        f.h.sample.wrapping_add(u64::from(f.h.block))
    }

    fn frame_at(&mut self, offset: i64) -> Option<Frame> {
        let mut head = [0; HEADER_MAX_BYTES];
        let want = (self.data_end - offset).min(HEADER_MAX_BYTES as i64).max(0) as usize;
        if want == 0 || !self.reader.read_exact_at(offset, &mut head[..want]) {
            return None;
        }
        self.parse_header(&head[..want]).map(|h| Frame { offset, h })
    }

    fn resync(&mut self) -> Result<()> {
        let Some(frame) = self.frame_at_or_after(self.cursor + 1, self.data_end) else {
            self.cursor = self.data_end;
            return Ok(());
        };
        warn!(TAG, "skipped {} bytes to the next frame", frame.offset - self.cursor);
        self.cursor = frame.offset;
        self.reader.seek(self.cursor);
        Ok(())
    }
}

impl Demuxer for FlacDemuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        while self.cursor < self.data_end {
            let remaining = (self.data_end - self.cursor) as usize;
            let mut window = if self.max_frame != 0 { self.max_frame as usize + HEADER_MAX_BYTES } else { WINDOW_BYTES };
            window = window.min(self.max_view);
            let found = loop {
                window = window.min(remaining);
                let mut data = self.reader.view_at(self.cursor, window)?;
                let Some(h) = self.parse_header(&data) else {
                    break None;
                };
                let expect = h.sample.wrapping_add(u64::from(h.block));
                let usable = if window == remaining { window } else { window - (HEADER_MAX_BYTES - 1) };
                let size = match self.scan_span(&data, usize::from(h.length), usable, Some(expect)) {
                    Some((size, _)) => size,
                    None if window < remaining && window < self.max_view => {
                        window = (window * 2).min(self.max_view);
                        continue;
                    }
                    None if window < remaining => break None,
                    None => window,
                };
                data.truncate(size);
                break Some((h, data, size));
            };
            let Some((h, data, size)) = found else {
                self.resync()?;
                continue;
            };
            let pts_us = (h.sample.wrapping_mul(1_000_000) / u64::from(self.sample_rate)) as i64;
            self.cursor += size as i64;
            self.reader.seek(self.cursor);
            return Ok(Some(Packet::audio(pts_us, data)));
        }
        Ok(None)
    }

    /// Lands on the start of the frame that holds `pts_us`.
    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        let target = if pts_us > 0 { (pts_us as u64).wrapping_mul(u64::from(self.sample_rate)) / 1_000_000 } else { 0 };

        let mut lo = self.frame_at(self.data_start).ok_or(Error::NotSeekable)?;
        let mut hi_offset = self.data_end;
        let mut hi_sample = self.total_samples;
        for i in 0..self.points.len() {
            let point = self.points[i];
            let offset = point.offset as i64;
            if point.sample <= target && point.sample > lo.h.sample && offset < self.data_end {
                if let Some(f) = self.frame_at(offset).filter(|f| f.h.sample == point.sample) {
                    lo = f;
                    continue;
                }
            }
            if point.sample > target && offset < hi_offset {
                hi_offset = offset;
                hi_sample = point.sample;
            }
        }

        let linear = self.frame_reach();
        for _ in 0..SEEK_STEPS {
            if target < lo.h.sample.wrapping_add(u64::from(lo.h.block)) {
                break;
            }
            let span = hi_offset - lo.offset;
            if span <= linear || hi_sample <= lo.h.sample {
                break;
            }
            let share = (target - lo.h.sample) as f64 / (hi_sample - lo.h.sample) as f64 * span as f64;
            let guess = (lo.offset + share as i64).max(lo.offset + 1).min(hi_offset - 1);
            let Some(f) = self.frame_at_or_after(guess, hi_offset) else {
                hi_offset = guess;
                continue;
            };
            if f.h.sample > target {
                hi_offset = f.offset;
                hi_sample = f.h.sample;
            } else {
                lo = f;
            }
        }

        while target >= lo.h.sample.wrapping_add(u64::from(lo.h.block)) {
            match self.next_frame(&lo) {
                Ok(next) => lo = next,
                Err(_) => break,
            }
        }

        self.cursor = lo.offset;
        self.reader.seek(self.cursor);
        Ok((lo.h.sample.wrapping_mul(1_000_000) / u64::from(self.sample_rate)) as i64)
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}
