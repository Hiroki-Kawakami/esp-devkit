/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::mem;

use idf::heap::{self, Caps};
use idf::log::{info, warn};

use crate::buffer::{Arena, Buffer, View, psram_first};
use crate::demux::{AudioCodec, AudioTrack, Container, Demuxer, Info, OpenOptions, Packet};
use crate::error::{Error, Result};
use crate::tags::Cover;
use crate::vorbis::{self, TagStream};

const TAG: &CStr = c"media_ogg";

pub const OPUS_HEAD_BYTES: usize = 19;

const HEADER_BYTES: usize = 27;
const MAX_PAGE_BYTES: i64 = HEADER_BYTES as i64 + 255 + 255 * 255;
const SCAN_BYTES: usize = 4096;
const SEEK_STEPS: usize = 48;
const MAX_PACKET_BYTES: usize = 256 * 1024;
const OPUS_RATE: i64 = 48000;

const PAGE_CONTINUED: u8 = 1;
const PAGE_BOS: u8 = 2;

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            r = if r & 0x8000_0000 != 0 { r << 1 ^ 0x04C1_1DB7 } else { r << 1 };
            bit += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
};

fn crc_update(crc: u32, p: &[u8]) -> u32 {
    p.iter().fold(crc, |crc, &byte| crc << 8 ^ CRC_TABLE[((crc >> 24) ^ u32::from(byte)) as usize])
}

#[derive(Clone)]
struct Page {
    offset: i64,
    body: i64,
    body_size: u32,
    granule: i64,
    serial: u32,
    flags: u8,
    segments: u8,
    lacing: [u8; 255],
}

impl Page {
    const fn new() -> Self {
        Self { offset: 0, body: 0, body_size: 0, granule: 0, serial: 0, flags: 0, segments: 0, lacing: [0; 255] }
    }

    fn end(&self) -> i64 {
        self.body + i64::from(self.body_size)
    }

    fn lacing(&self) -> &[u8] {
        &self.lacing[..usize::from(self.segments)]
    }

    fn completed_packets(&self) -> usize {
        self.lacing().iter().filter(|&&v| v < 255).count()
    }
}

#[derive(Clone)]
struct Cursor {
    page: Page,
    have_page: bool,
    seg: usize,
    seg_at: i64,
    next_page: i64,
    skip_packets: usize,
    drop_continued: bool,
    next_sample: i64,
}

impl Cursor {
    const fn at(next_page: i64, next_sample: i64) -> Self {
        Self { page: Page::new(), have_page: false, seg: 0, seg_at: 0, next_page, skip_packets: 0, drop_continued: false, next_sample }
    }

    fn more_completed(&self) -> bool {
        self.page.lacing()[self.seg..].iter().any(|&v| v < 255)
    }

    fn settle_time(&mut self) {
        if !self.more_completed() && self.page.granule >= 0 {
            self.next_sample = self.page.granule;
        }
    }
}

enum Gather<T> {
    Done(T),
    Dropped,
    Failed,
}

fn opus_samples(toc: &[u8]) -> u32 {
    let Some(&first) = toc.first() else {
        return 0;
    };
    let frame = if first & 0x80 != 0 {
        120 << ((first >> 3) & 3)
    } else if first & 0x60 == 0x60 {
        if first & 0x08 != 0 { 960 } else { 480 }
    } else {
        let size = (first >> 3) & 3;
        if size == 3 { 2880 } else { 480 << size }
    };
    match first & 3 {
        0 => frame,
        1 | 2 => 2 * frame,
        _ => toc.get(1).map_or(0, |&second| frame * u32::from(second & 0x3F)),
    }
}

/// Ogg Opus (RFC 7845), the first logical stream of the file only. A packet
/// that spans pages is a copy of its own; any other is a view. The first packets
/// come out early, timed from 0 after the pre-skip.
pub struct OggDemuxer {
    reader: Buffer,
    info: Info,
    serial: u32,
    pre_skip: u16,
    data_start: i64,
    data_end: i64,
    start_sample: i64,
    cur: Cursor,
    scan: Box<[u8; SCAN_BYTES], Caps>,
}

struct Packetized {
    start: i64,
    samples: u32,
    data: Option<View>,
}

impl OggDemuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let scan = psram_first(heap::zeroed_box)?;
        let data_end = reader.size();
        let mut demux = Self {
            reader,
            info: Info::new(Container::Ogg),
            serial: 0,
            pre_skip: 0,
            data_start: 0,
            data_end,
            start_sample: 0,
            cur: Cursor::at(0, 0),
            scan,
        };

        let mut first = Page::new();
        if !demux.load_page(0, &mut first) || first.flags & PAGE_BOS == 0 || first.segments == 0 {
            return Err(Error::Format("not an Ogg file"));
        }
        demux.serial = first.serial;
        let mut head = [0; OPUS_HEAD_BYTES];
        if (first.body_size as usize) < head.len() || !demux.reader.read_exact_at(first.body, &mut head) || !head.starts_with(b"OpusHead") {
            return Err(Error::Format("only Opus is supported in Ogg"));
        }
        demux.pre_skip = u16::from_le_bytes([head[10], head[11]]);

        let mut c = Cursor::at(first.end(), 0);
        let Some(data_start) = demux.read_opus_tags(&mut c, options.want_cover) else {
            return Err(Error::Format("no OpusTags in this Ogg file"));
        };
        demux.data_start = data_start;

        let mut probe = Cursor::at(demux.data_start, 0);
        if !demux.enter_page(&mut probe) {
            return Err(Error::Format("no Opus audio in this Ogg file"));
        }
        let mut start = probe.page.granule;
        if start >= 0 {
            let mut walk = probe.clone();
            for _ in 0..probe.page.completed_packets() {
                let Some(packet) = demux.next_packet(&mut walk, false) else {
                    break;
                };
                start -= i64::from(packet.samples);
            }
        }
        demux.start_sample = start.max(0);
        let end_sample = demux.find_end_sample();

        let total = end_sample.wrapping_sub(demux.start_sample).wrapping_sub(i64::from(demux.pre_skip));
        demux.info.duration_us = if total > 0 { total.wrapping_mul(1_000_000) / OPUS_RATE } else { 0 };
        let bitrate_bps = if demux.info.duration_us > 0 {
            ((demux.data_end - demux.data_start) as u64 * 8 * 1_000_000 / demux.info.duration_us as u64) as u32
        } else {
            0
        };
        let mut codec_private = Vec::new();
        codec_private.extend_from_slice(&head);
        demux.info.audio = Some(AudioTrack {
            codec: AudioCodec::Opus,
            sample_rate: OPUS_RATE as u32,
            bitrate_bps,
            channels: head[9],
            bits_per_sample: 0,
            block_align: 0,
            max_frame_bytes: 0,
            codec_private: codec_private.into_boxed_slice(),
        });

        info!(TAG, "{}: Opus x{}, pre-skip {}, {} us", path.to_str().unwrap_or("?"), head[9], demux.pre_skip, demux.info.duration_us);

        demux.cur = Cursor::at(demux.data_start, demux.start_sample);
        demux.reader.seek(demux.data_start);
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn load_page(&self, at: i64, page: &mut Page) -> bool {
        let mut h = [0; HEADER_BYTES];
        if at + HEADER_BYTES as i64 > self.data_end || !self.reader.read_exact_at(at, &mut h) {
            return false;
        }
        if !h.starts_with(b"OggS") || h[4] != 0 {
            return false;
        }
        page.offset = at;
        page.flags = h[5];
        page.granule = i64::from_le_bytes([h[6], h[7], h[8], h[9], h[10], h[11], h[12], h[13]]);
        page.serial = u32::from_le_bytes([h[14], h[15], h[16], h[17]]);
        page.segments = h[26];
        let segments = usize::from(page.segments);
        if !self.reader.read_exact_at(at + HEADER_BYTES as i64, &mut page.lacing[..segments]) {
            return false;
        }
        page.body = at + HEADER_BYTES as i64 + i64::from(page.segments);
        page.body_size = page.lacing().iter().map(|&v| u32::from(v)).sum();
        page.end() <= self.data_end
    }

    fn page_crc_ok(&mut self, page: &Page) -> bool {
        let mut h = [0; HEADER_BYTES];
        if !self.reader.read_exact_at(page.offset, &mut h) {
            return false;
        }
        let stored = u32::from_le_bytes([h[22], h[23], h[24], h[25]]);
        h[22..26].fill(0);
        let mut crc = crc_update(0, &h);
        crc = crc_update(crc, page.lacing());
        let end = page.end();
        let mut at = page.body;
        while at < end {
            let n = (end - at).min(SCAN_BYTES as i64) as usize;
            if !self.reader.read_exact_at(at, &mut self.scan[..n]) {
                return false;
            }
            crc = crc_update(crc, &self.scan[..n]);
            at += n as i64;
        }
        crc == stored
    }

    // First page of this stream starting in [from, limit) that passes its CRC;
    // "OggS" turns up in compressed data too.
    fn find_page(&mut self, from: i64, limit: i64, page: &mut Page) -> bool {
        let limit = limit.min(self.data_end);
        let mut at = from;
        while at < limit {
            let want = (self.data_end - at).min(SCAN_BYTES as i64) as usize;
            if want < 4 || !self.reader.read_exact_at(at, &mut self.scan[..want]) {
                return false;
            }
            let mut usable = want - 3;
            if at + usable as i64 > limit {
                usable = (limit - at) as usize;
            }
            for i in 0..usable {
                if self.scan[i] != b'O' || &self.scan[i..i + 4] != b"OggS" {
                    continue;
                }
                let candidate = at + i as i64;
                if self.load_page(candidate, page) && page.serial == self.serial && self.page_crc_ok(page) {
                    return true;
                }
                if !self.reader.read_exact_at(at, &mut self.scan[..want]) {
                    return false;
                }
            }
            at += usable as i64;
        }
        false
    }

    fn next_own_page(&mut self, mut at: i64, page: &mut Page) -> bool {
        while at < self.data_end {
            if !self.load_page(at, page) && !self.find_page(at + 1, self.data_end, page) {
                return false;
            }
            if page.serial == self.serial {
                return true;
            }
            if page.flags & PAGE_BOS != 0 && self.data_start != 0 && page.offset >= self.data_start {
                return false;
            }
            at = page.end();
        }
        false
    }

    fn sample_to_us(&self, sample: i64) -> i64 {
        sample.wrapping_sub(self.start_sample).wrapping_sub(i64::from(self.pre_skip)).wrapping_mul(1_000_000) / OPUS_RATE
    }

    fn enter_page(&mut self, c: &mut Cursor) -> bool {
        let mut page = mem::replace(&mut c.page, Page::new());
        let found = self.next_own_page(c.next_page, &mut page);
        c.page = page;
        if !found {
            return false;
        }
        c.have_page = true;
        c.seg = 0;
        c.seg_at = c.page.body;
        c.next_page = c.page.end();
        if c.drop_continued && c.page.flags & PAGE_CONTINUED != 0 {
            while c.seg < usize::from(c.page.segments) {
                let v = c.page.lacing[c.seg];
                c.seg += 1;
                c.seg_at += i64::from(v);
                if v < 255 {
                    c.skip_packets = c.skip_packets.saturating_sub(1);
                    break;
                }
            }
        }
        c.drop_continued = false;
        true
    }

    fn gather(&mut self, c: &mut Cursor) -> Gather<Box<[u8], Caps>> {
        let len: usize = c.page.lacing()[c.seg..].iter().map(|&v| usize::from(v)).sum();
        let cap = (len * 2).max(4096);
        let Ok(mut buf) = psram_first(|caps| heap::try_vec::<u8>(cap, caps)) else {
            return Gather::Failed;
        };
        buf.resize(len, 0);
        if !self.reader.read_exact_at(c.seg_at, &mut buf) {
            return Gather::Failed;
        }
        loop {
            if !self.enter_page(c) {
                return Gather::Failed;
            }
            if c.page.flags & PAGE_CONTINUED == 0 {
                warn!(TAG, "dropped a packet cut short at {}", c.page.offset);
                return Gather::Dropped;
            }
            let mut part = 0;
            let mut done = false;
            while c.seg < usize::from(c.page.segments) {
                let v = c.page.lacing[c.seg];
                c.seg += 1;
                part += usize::from(v);
                if v < 255 {
                    done = true;
                    break;
                }
            }
            let len = buf.len();
            if len + part > MAX_PACKET_BYTES || buf.try_reserve(part).is_err() {
                return Gather::Failed;
            }
            buf.resize(len + part, 0);
            if !self.reader.read_exact_at(c.seg_at, &mut buf[len..]) {
                return Gather::Failed;
            }
            c.seg_at += part as i64;
            if done {
                return Gather::Done(buf.into_boxed_slice());
            }
        }
    }

    fn skip_spanning(&mut self, c: &mut Cursor, toc: &mut [u8; 2]) -> Gather<()> {
        if !self.reader.read_exact_at(c.seg_at, toc) {
            return Gather::Failed;
        }
        loop {
            if !self.enter_page(c) {
                return Gather::Failed;
            }
            if c.page.flags & PAGE_CONTINUED == 0 {
                return Gather::Dropped;
            }
            while c.seg < usize::from(c.page.segments) {
                let v = c.page.lacing[c.seg];
                c.seg += 1;
                c.seg_at += i64::from(v);
                if v < 255 {
                    return Gather::Done(());
                }
            }
        }
    }

    // The next packet at `c`. Without `want_data` only its timing is worked
    // out, from its first two bytes.
    fn next_packet(&mut self, c: &mut Cursor, want_data: bool) -> Option<Packetized> {
        loop {
            if !c.have_page && !self.enter_page(c) {
                return None;
            }
            if c.seg >= usize::from(c.page.segments) {
                c.have_page = false;
                continue;
            }
            let at = c.seg_at;
            let mut len = 0;
            let mut complete = false;
            let mut s = c.seg;
            while s < usize::from(c.page.segments) {
                let v = c.page.lacing[s];
                s += 1;
                len += usize::from(v);
                if v < 255 {
                    complete = true;
                    break;
                }
            }

            let mut toc = [0; 2];
            let mut data = None;
            if complete {
                c.seg = s;
                c.seg_at = at + len as i64;
                if c.skip_packets > 0 {
                    c.skip_packets -= 1;
                    c.settle_time();
                    continue;
                }
                if len == 0 {
                    continue;
                }
                if want_data {
                    data = Some(self.reader.view_at(at, len).ok()?);
                } else if !self.reader.read_exact_at(at, &mut toc[..len.min(2)]) {
                    return None;
                }
            } else if want_data {
                match self.gather(c) {
                    Gather::Failed => return None,
                    Gather::Dropped => continue,
                    Gather::Done(copy) => {
                        len = copy.len();
                        data = Some(View::owned(copy));
                    }
                }
            } else {
                match self.skip_spanning(c, &mut toc) {
                    Gather::Failed => return None,
                    Gather::Dropped => continue,
                    Gather::Done(()) => {}
                }
            }

            let samples = match &data {
                Some(data) => opus_samples(data),
                None => opus_samples(&toc[..len.min(2)]),
            };
            let start = c.next_sample;
            c.next_sample += i64::from(samples);
            c.settle_time();
            return Some(Packetized { start, samples, data });
        }
    }

    // The OpusTags packet, read where it lies: it can span pages. Audio starts
    // on the page after the one it ends on, which is returned.
    fn read_opus_tags(&mut self, c: &mut Cursor, want_cover: bool) -> Option<i64> {
        let mut probe = c.clone();
        let mut len = 0u32;
        'pages: loop {
            if !self.enter_page(&mut probe) {
                return None;
            }
            for &v in probe.page.lacing() {
                len += u32::from(v);
                if v < 255 {
                    break 'pages;
                }
            }
        }
        let data_start = probe.next_page;
        if !self.enter_page(c) {
            return None;
        }
        let mut tags = mem::take(&mut self.info.tags);
        let mut stream = OggTags { demux: self, c, seg_left: 0 };
        let mut magic = [0; 8];
        if len < 8 || !stream.read(&mut magic) || magic != *b"OpusTags" {
            return None;
        }
        vorbis::read_comment_stream(&mut stream, len - 8, &mut tags, want_cover);
        tags.cover_scanned = want_cover;
        self.info.tags = tags;
        Some(data_start)
    }

    fn find_end_sample(&mut self) -> i64 {
        let mut window = MAX_PAGE_BYTES;
        loop {
            let from = (self.data_end - window).max(self.data_start);
            let mut last = -1;
            let mut page = Page::new();
            let mut at = from;
            while self.find_page(at, self.data_end, &mut page) {
                if page.granule >= 0 {
                    last = page.granule;
                }
                at = page.end();
            }
            if last >= 0 || from == self.data_start {
                return last;
            }
            window *= 2;
        }
    }
}

struct OggTags<'a> {
    demux: &'a mut OggDemuxer,
    c: &'a mut Cursor,
    seg_left: u32,
}

impl OggTags<'_> {
    fn advance(&mut self) -> bool {
        while self.seg_left == 0 {
            if self.c.seg >= usize::from(self.c.page.segments) && !self.demux.enter_page(self.c) {
                return false;
            }
            if self.c.seg >= usize::from(self.c.page.segments) {
                continue;
            }
            self.seg_left = u32::from(self.c.page.lacing[self.c.seg]);
            self.c.seg += 1;
        }
        true
    }

    fn step(&mut self, size: usize) -> Option<usize> {
        self.advance().then(|| size.min(self.seg_left as usize))
    }

    fn moved(&mut self, n: usize) {
        self.c.seg_at += n as i64;
        self.seg_left -= n as u32;
    }
}

impl TagStream for OggTags<'_> {
    fn read(&mut self, mut out: &mut [u8]) -> bool {
        while !out.is_empty() {
            let Some(n) = self.step(out.len()) else {
                return false;
            };
            let (now, rest) = out.split_at_mut(n);
            if !self.demux.reader.read_exact_at(self.c.seg_at, now) {
                return false;
            }
            self.moved(n);
            out = rest;
        }
        true
    }

    fn skip(&mut self, mut size: usize) -> bool {
        while size != 0 {
            let Some(n) = self.step(size) else {
                return false;
            };
            self.moved(n);
            size -= n;
        }
        true
    }
}

impl Demuxer for OggDemuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        let mut c = self.cur.clone();
        let Some(packet) = self.next_packet(&mut c, true) else {
            self.reader.check()?;
            return Ok(None);
        };
        self.cur = c;
        Ok(packet.data.map(|data| Packet::audio(self.sample_to_us(packet.start), data)))
    }

    /// Lands on the start of the packet that holds `pts_us`. The decoder needs
    /// about 80 ms before it to settle; that is the caller's, which seeks earlier
    /// and drops what it does not want.
    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        let wanted = if pts_us > 0 { pts_us.wrapping_mul(OPUS_RATE) / 1_000_000 } else { 0 };
        let target = self.start_sample + i64::from(self.pre_skip) + wanted;

        let mut lo: Option<Page> = None;
        let (mut lo_offset, mut hi_offset) = (self.data_start, self.data_end);
        let mut page = Page::new();
        for _ in 0..SEEK_STEPS {
            if hi_offset - lo_offset <= MAX_PAGE_BYTES {
                break;
            }
            let mid = lo_offset + (hi_offset - lo_offset) / 2;
            if !self.find_page(mid, hi_offset, &mut page) {
                hi_offset = mid;
                continue;
            }
            if page.granule >= 0 && page.granule <= target {
                lo_offset = page.offset;
                lo = Some(page.clone());
            } else {
                hi_offset = page.offset;
            }
        }
        let mut at = lo_offset;
        while at < self.data_end && self.next_own_page(at, &mut page) {
            if page.granule > target {
                break;
            }
            if page.granule >= 0 {
                lo = Some(page.clone());
            }
            at = page.end();
        }

        let mut c = Cursor::at(self.data_start, self.start_sample);
        if let Some(lo) = &lo {
            c.next_page = lo.offset;
            c.skip_packets = lo.completed_packets();
            c.drop_continued = true;
            c.next_sample = lo.granule;
        }
        loop {
            let mut probe = c.clone();
            let Some(packet) = self.next_packet(&mut probe, false) else {
                break;
            };
            if packet.start + i64::from(packet.samples) > target {
                break;
            }
            c = probe;
        }
        self.reader.seek(if c.have_page { c.seg_at } else { c.next_page });
        let landed = self.sample_to_us(c.next_sample).max(0);
        self.cur = c;
        Ok(landed)
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}
