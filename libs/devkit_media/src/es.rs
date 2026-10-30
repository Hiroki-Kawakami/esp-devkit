/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;

use idf::heap::{self, Caps};
use idf::log::info;

use crate::buffer::{Arena, Buffer, psram_first};
use crate::demux::{AudioCodec, AudioTrack, Container, Demuxer, Info, OpenOptions, Packet};
use crate::error::{Error, Result};
use crate::id3;
use crate::tags::Cover;

const TAG: &CStr = c"media_es";

const HEADER_BYTES: usize = 8;
const SCAN_BYTES: usize = 4096;
const SCAN_LIMIT: i64 = 256 * 1024;
const VERIFY_FRAMES: usize = 3;
const ESTIMATE_FRAMES: usize = 64;
const TOC_ENTRIES: usize = 100;

const BITRATE_V1_L2: [u16; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
const BITRATE_V1: [u16; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BITRATE_V2: [u16; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
const RATES: [[u32; 3]; 4] = [[11025, 12000, 8000], [0, 0, 0], [22050, 24000, 16000], [44100, 48000, 32000]];
const ADTS_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

#[derive(Clone, Copy, Default)]
struct Frame {
    size: u32,
    samples: u32,
    sample_rate: u32,
    channels: u8,
    layer: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Mpeg,
    Adts,
}

fn parse_mpeg(h: &[u8; HEADER_BYTES]) -> Option<Frame> {
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = usize::from((h[1] >> 3) & 3);
    let layer = (h[1] >> 1) & 3;
    if version == 1 || (layer != 1 && layer != 2) || (version == 0 && layer == 2) {
        return None;
    }

    let bitrate_index = usize::from(h[2] >> 4);
    let rate_index = usize::from((h[2] >> 2) & 3);
    if bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }

    let version1 = version == 3;
    let layer2 = layer == 2;
    let table = if !version1 {
        &BITRATE_V2
    } else if layer2 {
        &BITRATE_V1_L2
    } else {
        &BITRATE_V1
    };
    let bitrate = u32::from(table[bitrate_index]) * 1000;
    let rate = RATES[version][rate_index];
    if rate == 0 {
        return None;
    }

    let padding = u32::from((h[2] >> 1) & 1);
    let samples = if version1 || layer2 { 1152 } else { 576 };
    let size = samples / 8 * bitrate / rate + padding;
    (size > 4).then_some(Frame {
        size,
        samples,
        sample_rate: rate,
        channels: if (h[3] >> 6) & 3 == 3 { 1 } else { 2 },
        layer: if layer2 { 2 } else { 3 },
    })
}

fn parse_adts(h: &[u8; HEADER_BYTES]) -> Option<Frame> {
    if h[0] != 0xFF || h[1] & 0xF6 != 0xF0 {
        return None;
    }
    let rate_index = usize::from((h[2] >> 2) & 0x0F);
    if rate_index >= ADTS_RATES.len() {
        return None;
    }
    let channels = ((h[2] & 1) << 2) | (h[3] >> 6);
    if channels == 0 || channels > 2 {
        return None;
    }
    let length = u32::from(h[3] & 3) << 11 | u32::from(h[4]) << 3 | u32::from(h[5]) >> 5;
    if length < if h[1] & 1 != 0 { 7 } else { 9 } {
        return None;
    }
    Some(Frame {
        size: length,
        samples: 1024 * (u32::from(h[6] & 3) + 1),
        sample_rate: ADTS_RATES[rate_index],
        channels,
        layer: 0,
    })
}

pub struct EsDemuxer {
    reader: Buffer,
    info: Info,
    syntax: Syntax,
    sample_rate: u32,
    data_start: i64,
    data_end: i64,
    cursor: i64,
    samples: u64,
    toc: Option<[u8; TOC_ENTRIES]>,
    // Off the stack: every task that opens one of these files would otherwise
    // carry 4 KB it needs nowhere else.
    scan: Box<[u8; SCAN_BYTES], Caps>,
}

impl EsDemuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let scan = psram_first(heap::zeroed_box)?;
        let mut demux = Self {
            reader,
            info: Info::new(Container::Es),
            syntax: Syntax::Mpeg,
            sample_rate: 0,
            data_start: 0,
            data_end: 0,
            cursor: 0,
            samples: 0,
            toc: None,
            scan,
        };
        demux.info.tags.cover_scanned = true;

        let file_end = demux.reader.size();
        demux.data_start = id3::read_v2(&demux.reader, 0, &mut demux.info.tags, options.want_cover).unwrap_or(0);
        demux.data_end = demux.trim_tail(file_end);
        id3::read_v1(&demux.reader, file_end, &mut demux.info.tags);
        if demux.data_end <= demux.data_start {
            return Err(Error::Format("this file carries no audio"));
        }

        let Some((first, frame)) = demux.detect() else {
            return Err(Error::Format("no MP3, MP2 or AAC frames in this file"));
        };
        let codec = match (demux.syntax, frame.layer) {
            (Syntax::Adts, _) => AudioCodec::Aac,
            (_, 2) => AudioCodec::Mp2,
            _ => AudioCodec::Mp3,
        };
        demux.data_start = first;
        demux.sample_rate = frame.sample_rate;
        demux.cursor = first;

        let bytes = demux.data_end - demux.data_start;
        let counted = if codec == AudioCodec::Mp3 { demux.read_vbr_header(first, &frame) } else { None };
        if let Some(frames) = counted {
            demux.info.duration_us = (frames.wrapping_mul(u64::from(frame.samples)).wrapping_mul(1_000_000) / u64::from(frame.sample_rate)) as i64;
            demux.cursor = first + i64::from(frame.size);
            demux.data_start = demux.cursor;
        } else {
            demux.info.duration_us = demux.estimate_duration_us(first, bytes);
            demux.info.duration_exact = false;
        }
        let bitrate_bps = if demux.info.duration_us > 0 {
            ((demux.data_end - demux.data_start) as u64 * 8 * 1_000_000 / demux.info.duration_us as u64) as u32
        } else {
            0
        };
        demux.info.audio = Some(AudioTrack {
            codec,
            sample_rate: frame.sample_rate,
            bitrate_bps,
            channels: frame.channels,
            bits_per_sample: 0,
            block_align: 0,
            max_frame_bytes: 0,
            codec_private: Box::default(),
        });

        info!(TAG, "{}: {} {} Hz x{}, {} us{}", path.to_str().unwrap_or("?"), codec.name(), frame.sample_rate,
              frame.channels, demux.info.duration_us, if demux.info.duration_exact { "" } else { " (estimated)" });

        demux.reader.seek(demux.cursor);
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn detect(&mut self) -> Option<(i64, Frame)> {
        for syntax in [Syntax::Mpeg, Syntax::Adts] {
            self.syntax = syntax;
            if let Some(found) = self.resync(self.data_start) {
                return Some(found);
            }
        }
        None
    }

    fn parse_header(&self, h: &[u8; HEADER_BYTES]) -> Option<Frame> {
        match self.syntax {
            Syntax::Adts => parse_adts(h),
            Syntax::Mpeg => parse_mpeg(h),
        }
    }

    fn read_header(&self, offset: i64) -> Option<Frame> {
        let mut header = [0; HEADER_BYTES];
        self.reader.read_exact_at(offset, &mut header).then(|| self.parse_header(&header)).flatten()
    }

    fn frames_chain(&self, mut offset: i64) -> Option<Frame> {
        let first = self.read_header(offset)?;
        offset += i64::from(first.size);
        if offset > self.data_end {
            return None;
        }
        for _ in 1..VERIFY_FRAMES {
            if offset == self.data_end {
                break;
            }
            let frame = self.read_header(offset)?;
            if frame.layer != first.layer {
                return None;
            }
            offset += i64::from(frame.size);
            if offset > self.data_end {
                return None;
            }
        }
        Some(first)
    }

    fn resync(&mut self, mut from: i64) -> Option<(i64, Frame)> {
        let mut scanned = 0;
        while from < self.data_end && scanned < SCAN_LIMIT {
            self.reader.seek(from);
            let want = ((self.data_end - from).min(SCAN_BYTES as i64)) as usize;
            let got = self.reader.read(&mut self.scan[..want]);
            if got < HEADER_BYTES {
                return None;
            }
            for i in 0..=got - HEADER_BYTES {
                if self.scan[i] != 0xFF {
                    continue;
                }
                let at = from + i as i64;
                if let Some(frame) = self.frames_chain(at) {
                    return Some((at, frame));
                }
            }
            from += (got - (HEADER_BYTES - 1)) as i64;
            scanned += got as i64;
        }
        None
    }

    fn trim_tail(&self, mut end: i64) -> i64 {
        let mut tail = [0; 32];
        if end >= 128 && self.reader.read_exact_at(end - 128, &mut tail[..3]) && tail.starts_with(b"TAG") {
            end -= 128;
        }
        if end >= 32 && self.reader.read_exact_at(end - 32, &mut tail) && tail.starts_with(b"APETAGEX") {
            let size = i64::from(u32::from_le_bytes([tail[12], tail[13], tail[14], tail[15]]));
            if size <= end {
                end -= size;
            }
            if tail[23] & 0x80 != 0 && end >= 32 {
                end -= 32;
            }
        }
        end
    }

    fn read_vbr_header(&mut self, offset: i64, frame: &Frame) -> Option<u64> {
        let mut buffer = [0; 192];
        let want = (frame.size as usize).min(buffer.len());
        if !self.reader.read_exact_at(offset, &mut buffer[..want]) {
            return None;
        }
        let word = |at: usize| u32::from_be_bytes([buffer[at], buffer[at + 1], buffer[at + 2], buffer[at + 3]]);

        let mut i = 4;
        while i + 12 <= want {
            let name = &buffer[i..i + 4];
            let xing = name == b"Xing" || name == b"Info";
            if !xing && name != b"VBRI" {
                i += 1;
                continue;
            }
            if !xing {
                if i + 20 > want {
                    return None;
                }
                return Some(u64::from(word(i + 14))).filter(|&n| n > 0);
            }
            let flags = word(i + 4);
            let mut position = i + 8;
            if flags & 1 == 0 || position + 4 > want {
                return None;
            }
            let frames = u64::from(word(position));
            position += 4;
            if flags & 2 != 0 {
                position += 4;
            }
            if flags & 4 != 0 && position + TOC_ENTRIES <= want {
                let mut toc = [0; TOC_ENTRIES];
                toc.copy_from_slice(&buffer[position..position + TOC_ENTRIES]);
                self.toc = Some(toc);
            }
            return Some(frames).filter(|&n| n > 0);
        }
        None
    }

    fn estimate_duration_us(&self, mut offset: i64, bytes: i64) -> i64 {
        let (mut frame_bytes, mut samples, mut rate) = (0u64, 0u64, 0u32);
        for _ in 0..ESTIMATE_FRAMES {
            if offset + HEADER_BYTES as i64 > self.data_end {
                break;
            }
            let Some(frame) = self.read_header(offset) else {
                break;
            };
            frame_bytes += u64::from(frame.size);
            samples += u64::from(frame.samples);
            rate = frame.sample_rate;
            offset += i64::from(frame.size);
        }
        if frame_bytes == 0 || rate == 0 {
            return 0;
        }
        ((bytes as u64).wrapping_mul(samples).wrapping_mul(1_000_000) / (frame_bytes * u64::from(rate))) as i64
    }

    fn byte_for_us(&self, pts_us: i64) -> i64 {
        let bytes = self.data_end - self.data_start;
        if self.info.duration_us <= 0 || bytes <= 0 {
            return self.data_start;
        }
        let mut ratio = (pts_us as f64 / self.info.duration_us as f64).clamp(0.0, 1.0);
        if let Some(toc) = &self.toc {
            let percent = ratio * 100.0;
            let index = percent as usize;
            let low = f64::from(toc[index.min(TOC_ENTRIES - 1)]);
            let high = if index + 1 < TOC_ENTRIES { f64::from(toc[index + 1]) } else { 256.0 };
            ratio = (low + (high - low) * (percent - index as f64)) / 256.0;
        }
        self.data_start + (ratio * bytes as f64) as i64
    }

    fn us_for_byte(&self, offset: i64) -> i64 {
        let bytes = self.data_end - self.data_start;
        if bytes <= 0 {
            return 0;
        }
        ((offset - self.data_start) as f64 / bytes as f64 * self.info.duration_us as f64) as i64
    }
}

impl Demuxer for EsDemuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        while self.cursor + HEADER_BYTES as i64 <= self.data_end {
            let frame = self.read_header(self.cursor).filter(|frame| self.cursor + i64::from(frame.size) <= self.data_end);
            let Some(frame) = frame else {
                let Some((found, _)) = self.resync(self.cursor + 1) else {
                    self.reader.check()?;
                    return Ok(None);
                };
                self.cursor = found;
                continue;
            };

            self.reader.seek(self.cursor);
            let data = self.reader.view(frame.size as usize)?;
            let pts_us = (self.samples.wrapping_mul(1_000_000) / u64::from(self.sample_rate)) as i64;
            self.cursor += i64::from(frame.size);
            self.samples += u64::from(frame.samples);
            return Ok(Some(Packet::audio(pts_us, data)));
        }
        Ok(None)
    }

    /// An estimate: only CBR lands where it says.
    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        let found = if pts_us <= 0 {
            let start = self.data_start;
            if self.frames_chain(start).is_some() {
                start
            } else {
                self.resync(start).ok_or(Error::NotSeekable)?.0
            }
        } else {
            self.resync(self.byte_for_us(pts_us)).ok_or(Error::NotSeekable)?.0
        };

        let landed = self.us_for_byte(found);
        self.cursor = found;
        self.samples = (landed as u64).wrapping_mul(u64::from(self.sample_rate)) / 1_000_000;
        self.reader.seek(found);
        Ok(landed)
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}
