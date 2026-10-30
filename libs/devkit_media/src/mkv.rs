/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::CStr;

use devkit::math;
use idf::heap::{self, Caps};
use idf::log::{info, warn};

use crate::alac;
use crate::buffer::{Aligned, Arena, Buffer, psram_first};
use crate::demux::{AudioCodec, AudioTrack, Container, Demuxer, Info, OpenOptions, Packet, PacketKind, VideoCodec, VideoTrack};
use crate::error::{Error, Result};
use crate::flac;
use crate::tags::{Cover, Encoding, Field};

const TAG: &CStr = c"media_mkv";

const ID_EBML: u32 = 0x1A45DFA3;
const ID_DOC_TYPE: u32 = 0x4282;
const ID_SEGMENT: u32 = 0x18538067;
const ID_SEEK_HEAD: u32 = 0x114D9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEK_ID: u32 = 0x53AB;
const ID_SEEK_POSITION: u32 = 0x53AC;
const ID_INFO: u32 = 0x1549A966;
const ID_TITLE: u32 = 0x7BA9;
const ID_TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TRACKS: u32 = 0x1654AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_DEFAULT_DURATION: u32 = 0x23E383;
const ID_CONTENT_ENCODINGS: u32 = 0x6D80;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;
const ID_PROJECTION: u32 = 0x7670;
const ID_PROJECTION_TYPE: u32 = 0x7671;
const ID_POSE_YAW: u32 = 0x7673;
const ID_POSE_PITCH: u32 = 0x7674;
const ID_POSE_ROLL: u32 = 0x7675;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQ: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_BIT_DEPTH: u32 = 0x6264;
const ID_CLUSTER: u32 = 0x1F43B675;
const ID_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_REFERENCE_BLOCK: u32 = 0xFB;
const ID_CUES: u32 = 0x1C53BB6B;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POS: u32 = 0xB7;
const ID_CUE_TRACK: u32 = 0xF7;
const ID_CUE_CLUSTER_POS: u32 = 0xF1;
const ID_TAGS: u32 = 0x1254C367;
const ID_TAG: u32 = 0x7373;
const ID_SIMPLE_TAG: u32 = 0x67C8;
const ID_TAG_NAME: u32 = 0x45A3;
const ID_TAG_STRING: u32 = 0x4487;
const ID_ATTACHMENTS: u32 = 0x1941A469;
const ID_ATTACHED_FILE: u32 = 0x61A7;
const ID_FILE_NAME: u32 = 0x466E;
const ID_FILE_DATA: u32 = 0x465C;

const TRACK_TYPE_VIDEO: u64 = 1;
const TRACK_TYPE_AUDIO: u64 = 2;

const LACING_MASK: u8 = 0x06;
const LACING_XIPH: u8 = 0x02;
const LACING_FIXED: u8 = 0x04;
const LACING_EBML: u8 = 0x06;
const MAX_LACES: usize = 256;

const UNKNOWN_SIZE: u64 = u64::MAX;
const MAX_EBML_HEADER_BYTES: usize = 4096;
const MAX_HEADER_BYTES: usize = 256 * 1024;
const MAX_CUE_POINT_BYTES: usize = 1024;
const MAX_TAGS_BYTES: usize = 256 * 1024;
const MAX_ATTACHMENTS_BYTES: usize = 4 * 1024 * 1024;
const MAX_CUES: usize = 60000;
const PROBE_BLOCKS: usize = 64;
const KEYFRAME_SCAN_BLOCKS: usize = 256;
const ROTATION_TOLERANCE_DEG: f64 = 1.0;
const POSE_TOLERANCE_DEG: f64 = 1.0;
const WAVE_FORMAT_BYTES: usize = 18;
const WAVE_FORMAT_IMA_ADPCM: u16 = 0x0011;
const CODEC_BYTES: usize = 32;
const NAME_BYTES: usize = 32;
const DOC_TYPE_BYTES: usize = 16;
const MAX_TAG_DEPTH: usize = 16;

#[derive(Clone, Copy)]
struct Cue {
    time_ms: u32,
    position: u32,
}

#[derive(Clone, Copy, Default)]
struct Block {
    track: u64,
    pts_us: i64,
    keyframe: bool,
    laced: bool,
    laces: u16,
    end: i64,
    payload: u32,
}

#[derive(Default)]
struct Track<'a> {
    number: u64,
    kind: u64,
    codec: &'a [u8],
    codec_private: Option<&'a [u8]>,
    default_duration_ns: u64,
    encoded: bool,
    width: u32,
    height: u32,
    projection_type: u64,
    yaw: f64,
    pitch: f64,
    roll: f64,
    sample_rate: f64,
    channels: u64,
    bits: u64,
}

fn vint_length(first: u8) -> usize {
    first.leading_zeros() as usize + 1
}

fn read_byte(reader: &Buffer) -> Option<u8> {
    let mut byte = [0; 1];
    reader.read_all(&mut byte).then_some(byte[0])
}

fn read_id(reader: &Buffer) -> Option<u32> {
    let byte = read_byte(reader)?;
    let length = vint_length(byte);
    if length > 4 {
        return None;
    }
    let mut value = u32::from(byte);
    for _ in 1..length {
        value = value << 8 | u32::from(read_byte(reader)?);
    }
    Some(value)
}

fn read_vint(reader: &Buffer) -> Option<u64> {
    let byte = read_byte(reader)?;
    let length = vint_length(byte);
    if length > 8 {
        return None;
    }
    let mask = (0xFFu32 >> length) as u8;
    let mut value = u64::from(byte & mask);
    let mut all_ones = value == u64::from(mask);
    for _ in 1..length {
        let byte = read_byte(reader)?;
        value = value << 8 | u64::from(byte);
        all_ones = all_ones && byte == 0xFF;
    }
    Some(if all_ones { UNKNOWN_SIZE } else { value })
}

fn read_header(reader: &Buffer) -> Option<(u32, u64)> {
    Some((read_id(reader)?, read_vint(reader)?))
}

fn span_vint(span: &mut &[u8], keep_marker: bool) -> Option<u64> {
    let (&first, _) = span.split_first()?;
    let length = vint_length(first);
    if length > 8 || length > span.len() {
        return None;
    }
    let (bytes, rest) = span.split_at(length);
    let lead = if keep_marker { first } else { first & (0xFFu32 >> length) as u8 };
    let value = bytes[1..].iter().fold(u64::from(lead), |v, &b| v << 8 | u64::from(b));
    *span = rest;
    Some(value)
}

fn span_next<'a>(span: &mut &'a [u8]) -> Option<(u32, &'a [u8])> {
    let id = span_vint(span, true)? as u32;
    let size = span_vint(span, false)?;
    if size > span.len() as u64 {
        return None;
    }
    let (body, rest) = span.split_at(size as usize);
    *span = rest;
    Some((id, body))
}

fn span_uint(body: &[u8]) -> u64 {
    body.iter().take(8).fold(0, |v, &b| v << 8 | u64::from(b))
}

fn span_float(body: &[u8]) -> f64 {
    match body.len() {
        4 => f64::from(f32::from_bits(span_uint(body) as u32)),
        8 => f64::from_bits(span_uint(body)),
        _ => 0.0,
    }
}

// What a C string of `capacity` bytes holds of the element.
fn span_string(body: &[u8], capacity: usize) -> &[u8] {
    let body = &body[..body.len().min(capacity - 1)];
    body.iter().position(|&b| b == 0).map_or(body, |end| &body[..end])
}

fn load_body(reader: &Buffer, size: u64, limit: usize) -> Option<Aligned> {
    if size == UNKNOWN_SIZE || size > limit as u64 {
        return None;
    }
    let mut data = Aligned::new(0, size as usize).ok()?;
    reader.read_all(&mut data).then_some(data)
}

fn rotation_of(track: &Track) -> u16 {
    if track.projection_type != 0 || track.yaw.abs() > POSE_TOLERANCE_DEG || track.pitch.abs() > POSE_TOLERANCE_DEG {
        return 0;
    }
    let quarters = math::round(track.roll / 90.0);
    if (track.roll - quarters * 90.0).abs() > ROTATION_TOLERANCE_DEG {
        warn!(TAG, "ignoring roll {:.1}, not a multiple of 90", track.roll);
        return 0;
    }
    let turns = (quarters as i32).rem_euclid(4);
    (turns * 90) as u16
}

fn copy_private(data: &[u8]) -> Box<[u8]> {
    let mut copy = Vec::new();
    if copy.try_reserve_exact(data.len()).is_ok() {
        copy.extend_from_slice(data);
    }
    copy.into_boxed_slice()
}

pub struct MkvDemuxer {
    reader: Buffer,
    info: Info,
    max_view: usize,
    segment_start: i64,
    segment_end: i64,
    first_cluster: i64,
    cues_offset: i64,
    tags_offset: i64,
    attachments_offset: i64,
    have_tags: bool,
    have_attachments: bool,
    want_cover: bool,
    want_audio: bool,
    timestamp_scale: u64,
    duration: f64,
    video_track: u64,
    audio_track: u64,
    default_duration_ns: u64,
    cluster_time: u64,
    skip_before_us: i64,
    audio_before_us: i64,
    need_keyframe: bool,
    group_end: i64,
    group_has_ref: bool,
    cues: Vec<Cue, Caps>,
    lace_sizes: Box<[u32; MAX_LACES], Caps>,
    lace_count: u16,
    lace_next: u16,
    lace_pts_us: i64,
    lace_end: i64,
}

impl MkvDemuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let max_view = reader.max_view();
        let lace_sizes = psram_first(heap::zeroed_box)?;
        let mut demux = Self {
            reader,
            info: Info::new(Container::Mkv),
            max_view,
            segment_start: 0,
            segment_end: 0,
            first_cluster: 0,
            cues_offset: 0,
            tags_offset: 0,
            attachments_offset: 0,
            have_tags: false,
            have_attachments: false,
            want_cover: options.want_cover,
            want_audio: true,
            timestamp_scale: 1_000_000,
            duration: 0.0,
            video_track: 0,
            audio_track: 0,
            default_duration_ns: 0,
            cluster_time: 0,
            skip_before_us: -1,
            audio_before_us: -1,
            need_keyframe: false,
            group_end: 0,
            group_has_ref: false,
            cues: Vec::new_in(Caps::DEFAULT),
            lace_sizes,
            lace_count: 0,
            lace_next: 0,
            lace_pts_us: 0,
            lace_end: 0,
        };
        demux.info.seekable = false;
        demux.info.tags.cover_scanned = options.want_cover;

        demux.read_ebml_header()?;
        demux.read_segment_headers()?;
        match demux.info.video.as_ref().map(|video| video.codec) {
            None => return Err(Error::Format("no video track")),
            Some(VideoCodec::Unsupported) => return Err(Error::Format("unsupported video codec in this MKV")),
            Some(_) => {}
        }

        if demux.cues_offset != 0 {
            demux.load_cues();
        }

        let interval = if demux.default_duration_ns != 0 {
            (demux.default_duration_ns / 1000) as i64
        } else {
            demux.probe_frame_interval()
        };
        let mut duration = (demux.duration * demux.timestamp_scale as f64 / 1000.0) as i64;
        if duration <= 0 {
            if let Some(last) = demux.cues.last() {
                duration = i64::from(last.time_ms) * 1000 + interval;
            }
        }
        demux.info.duration_us = duration;
        if let Some(video) = demux.info.video.as_mut() {
            video.frame_interval_us = interval;
            info!(TAG, "{}: {}x{}, rotation {}, {} us/frame, {} us, {}", path.to_str().unwrap_or("?"),
                  video.width, video.height, video.rotation_ccw, interval, duration,
                  if demux.info.seekable { "seekable" } else { "not seekable" });
        }

        demux.reader.seek(demux.first_cluster);
        demux.cluster_time = 0;
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn read_ebml_header(&self) -> Result<()> {
        const NOT_MKV: Error = Error::Format("not a Matroska file");
        match read_header(&self.reader) {
            Some((ID_EBML, size)) => {
                let data = load_body(&self.reader, size, MAX_EBML_HEADER_BYTES).ok_or(NOT_MKV)?;
                let mut span = &data[..];
                let mut doc_type: &[u8] = &[];
                while let Some((id, body)) = span_next(&mut span) {
                    if id == ID_DOC_TYPE {
                        doc_type = span_string(body, DOC_TYPE_BYTES);
                    }
                }
                if doc_type == b"matroska" || doc_type == b"webm" { Ok(()) } else { Err(NOT_MKV) }
            }
            _ => Err(NOT_MKV),
        }
    }

    fn read_segment_headers(&mut self) -> Result<()> {
        let size = match read_header(&self.reader) {
            Some((ID_SEGMENT, size)) => size,
            _ => return Err(Error::Format("no Segment in this MKV")),
        };
        let file_end = self.reader.size();
        self.segment_start = self.reader.tell();
        self.segment_end = file_end;
        if size != UNKNOWN_SIZE && (self.segment_start as u64).saturating_add(size) < file_end as u64 {
            self.segment_end = self.segment_start + size as i64;
        }

        while self.reader.tell() < self.segment_end {
            let position = self.reader.tell();
            let Some((id, size)) = read_header(&self.reader) else {
                break;
            };
            if id == ID_CLUSTER {
                self.first_cluster = position;
                self.read_deferred_tags();
                return Ok(());
            }
            if size == UNKNOWN_SIZE {
                return Err(Error::Format("unsupported MKV layout"));
            }
            let next = self.reader.tell().wrapping_add(size as i64);

            if id == ID_SEEK_HEAD || id == ID_INFO || id == ID_TRACKS {
                let data = load_body(&self.reader, size, MAX_HEADER_BYTES).ok_or(Error::Format("MKV header is too large"))?;
                match id {
                    ID_SEEK_HEAD => self.parse_seek_head(&data),
                    ID_INFO => self.parse_info(&data),
                    _ => self.parse_tracks(&data),
                }
            } else if id == ID_TAGS || id == ID_ATTACHMENTS {
                self.read_tag_element(position, id);
            } else if id == ID_CUES && self.cues_offset == 0 {
                self.cues_offset = position;
            }
            self.reader.seek(next);
        }
        Err(Error::Format("no clusters in this MKV"))
    }

    fn parse_seek_head(&mut self, mut span: &[u8]) {
        while let Some((id, mut body)) = span_next(&mut span) {
            if id != ID_SEEK {
                continue;
            }
            let (mut target, mut position) = (0, None);
            while let Some((child, value)) = span_next(&mut body) {
                if child == ID_SEEK_ID {
                    target = span_uint(value);
                }
                if child == ID_SEEK_POSITION {
                    position = Some(span_uint(value));
                }
            }
            let Some(position) = position else {
                continue;
            };
            let offset = self.segment_start.wrapping_add(position as i64);
            if target == u64::from(ID_CUES) && self.cues_offset == 0 {
                self.cues_offset = offset;
            }
            if target == u64::from(ID_TAGS) && self.tags_offset == 0 {
                self.tags_offset = offset;
            }
            if target == u64::from(ID_ATTACHMENTS) && self.attachments_offset == 0 {
                self.attachments_offset = offset;
            }
        }
    }

    fn parse_info(&mut self, mut span: &[u8]) {
        while let Some((id, body)) = span_next(&mut span) {
            match id {
                ID_TIMESTAMP_SCALE => {
                    let scale = span_uint(body);
                    if scale != 0 {
                        self.timestamp_scale = scale;
                    }
                }
                ID_DURATION => self.duration = span_float(body),
                ID_TITLE => self.info.tags.set(Field::Title, body, Encoding::Utf8),
                _ => {}
            }
        }
    }

    fn parse_tracks(&mut self, mut span: &[u8]) {
        while let Some((id, mut entry)) = span_next(&mut span) {
            if id != ID_TRACK_ENTRY {
                continue;
            }
            let mut track = Track { channels: 1, ..Default::default() };
            while let Some((child, body)) = span_next(&mut entry) {
                match child {
                    ID_TRACK_NUMBER => track.number = span_uint(body),
                    ID_TRACK_TYPE => track.kind = span_uint(body),
                    ID_CODEC_ID => track.codec = span_string(body, CODEC_BYTES),
                    ID_CODEC_PRIVATE => track.codec_private = Some(body),
                    ID_DEFAULT_DURATION => track.default_duration_ns = span_uint(body),
                    ID_CONTENT_ENCODINGS => track.encoded = true,
                    ID_VIDEO => parse_video(&mut track, body),
                    ID_AUDIO => parse_audio(&mut track, body),
                    _ => {}
                }
            }
            if track.number == 0 {
                continue;
            }
            if track.kind == TRACK_TYPE_VIDEO && self.video_track == 0 {
                self.use_video_track(&track);
            }
            if track.kind == TRACK_TYPE_AUDIO && self.audio_track == 0 {
                self.use_audio_track(&track);
            }
        }
    }

    fn use_video_track(&mut self, track: &Track) {
        self.video_track = track.number;
        self.default_duration_ns = track.default_duration_ns;
        let mut codec = VideoCodec::Unsupported;
        let mut codec_private = Box::default();
        if !track.encoded {
            match track.codec {
                b"V_MJPEG" => codec = VideoCodec::Mjpeg,
                b"V_MPEG4/ISO/AVC" | b"V_MPEG2" => {
                    codec = if track.codec == b"V_MPEG2" { VideoCodec::Mpeg2 } else { VideoCodec::H264 };
                    codec_private = copy_private(track.codec_private.unwrap_or(&[]));
                }
                _ => {}
            }
        }
        self.info.video = Some(VideoTrack {
            codec,
            width: track.width,
            height: track.height,
            frame_interval_us: 0,
            rotation_ccw: rotation_of(track),
            frame_count: 0,
            max_frame_bytes: 0,
            codec_private,
        });
    }

    fn use_audio_track(&mut self, track: &Track) {
        self.audio_track = track.number;
        let mut audio = AudioTrack {
            codec: AudioCodec::Unsupported,
            sample_rate: math::lround(track.sample_rate) as u32,
            bitrate_bps: 0,
            channels: track.channels as u8,
            bits_per_sample: if track.bits != 0 { track.bits as u8 } else { 16 },
            block_align: 0,
            max_frame_bytes: 0,
            codec_private: Box::default(),
        };
        let private = track.codec_private.unwrap_or(&[]);
        if !track.encoded {
            match track.codec {
                b"A_PCM/INT/LIT" => audio.codec = AudioCodec::Pcm,
                b"A_MPEG/L3" => audio.codec = AudioCodec::Mp3,
                b"A_MPEG/L2" => audio.codec = AudioCodec::Mp2,
                b"A_OPUS" => {
                    audio.codec = AudioCodec::Opus;
                    audio.codec_private = copy_private(private);
                }
                b"A_FLAC" => {
                    if let Some(si) = flac::find_streaminfo(private) {
                        audio.codec = AudioCodec::Flac;
                        (audio.sample_rate, audio.channels, audio.bits_per_sample) = flac::streaminfo_format(si);
                        audio.codec_private = copy_private(si);
                    }
                }
                b"A_ALAC" => {
                    if let Some(cookie) = alac::find_cookie(private) {
                        audio.codec = AudioCodec::Alac;
                        (audio.sample_rate, audio.channels, audio.bits_per_sample) = alac::cookie_format(cookie);
                        audio.codec_private = copy_private(cookie);
                    }
                }
                b"A_MS/ACM" => use_wave_format(&mut audio, private),
                codec if codec.starts_with(b"A_AAC") => {
                    audio.codec = AudioCodec::Aac;
                    audio.codec_private = copy_private(private);
                }
                _ => {}
            }
        }
        if audio.sample_rate == 0 || audio.channels == 0 {
            audio.codec = AudioCodec::Unsupported;
        }
        self.info.audio = Some(audio);
    }

    fn to_us(&self, timestamp: i64) -> i64 {
        timestamp.wrapping_mul(self.timestamp_scale as i64) / 1000
    }

    fn parse_cue_point(&self, mut span: &[u8]) -> Option<(u64, u64)> {
        let (mut time, mut position) = (None, None);
        while let Some((id, mut body)) = span_next(&mut span) {
            if id == ID_CUE_TIME {
                time = Some(span_uint(body));
            } else if id == ID_CUE_TRACK_POS && position.is_none() {
                let (mut track, mut cluster) = (0, None);
                while let Some((child, value)) = span_next(&mut body) {
                    if child == ID_CUE_TRACK {
                        track = span_uint(value);
                    }
                    if child == ID_CUE_CLUSTER_POS {
                        cluster = Some(span_uint(value));
                    }
                }
                if track == self.video_track {
                    position = cluster;
                }
            }
        }
        Some((time?, position?))
    }

    fn cluster_at(&self, offset: i64) -> bool {
        if offset >= self.segment_end {
            return false;
        }
        self.reader.seek(offset);
        read_id(&self.reader) == Some(ID_CLUSTER)
    }

    fn load_cues(&mut self) {
        self.reader.seek(self.cues_offset);
        let size = match read_header(&self.reader) {
            Some((ID_CUES, size)) if size != UNKNOWN_SIZE => size,
            _ => {
                warn!(TAG, "no Cues at the SeekHead position; seeking disabled");
                return;
            }
        };

        let capacity = (size / 8 + 1).min(MAX_CUES as u64) as usize;
        let Ok(mut cues) = psram_first(|caps| heap::try_vec::<Cue>(capacity, caps)) else {
            warn!(TAG, "no memory for the cue index; seeking disabled");
            return;
        };

        let dedup = self.video_codec() == VideoCodec::Mjpeg;
        let (mut step, mut seen) = (1u32, 0u32);
        let mut point = [0; MAX_CUE_POINT_BYTES];
        let end = self.reader.tell().wrapping_add(size as i64);
        self.reader.set_readahead(true);
        while self.reader.tell() < end {
            let Some((id, size)) = read_header(&self.reader).filter(|&(_, size)| size != UNKNOWN_SIZE) else {
                break;
            };
            let next = self.reader.tell().wrapping_add(size as i64);
            if id != ID_CUE_POINT || size > MAX_CUE_POINT_BYTES as u64 || !self.reader.read_all(&mut point[..size as usize]) {
                self.reader.seek(next);
                continue;
            }
            let Some((time, position)) = self.parse_cue_point(&point[..size as usize]) else {
                continue;
            };
            if position > u64::from(u32::MAX) {
                continue;
            }
            let position = position as u32;
            if dedup && cues.last().is_some_and(|cue| cue.position == position) {
                continue;
            }
            let index = seen;
            seen = seen.wrapping_add(1);
            if index % step != 0 {
                continue;
            }
            if cues.len() == capacity {
                let kept = cues.len() / 2;
                for i in 0..kept {
                    cues[i] = cues[i * 2];
                }
                cues.truncate(kept);
                step *= 2;
            }
            cues.push(Cue { time_ms: (self.to_us(time as i64) / 1000) as u32, position });
        }
        self.reader.set_readahead(false);

        let first = cues.first().map(|cue| self.segment_start + i64::from(cue.position));
        if !first.is_some_and(|offset| self.cluster_at(offset)) {
            warn!(TAG, "Cues do not point at clusters; seeking disabled");
            return;
        }
        self.cues = cues;
        self.info.seekable = true;
    }

    fn read_lacing(&mut self, lacing: u8, end: i64) -> Option<u16> {
        let reader = &self.reader;
        let sizes = &mut self.lace_sizes;
        let last = usize::from(read_byte(reader)?);
        let count = last + 1;

        let mut known = 0u64;
        let mut valid = true;
        if lacing == LACING_XIPH {
            for slot in &mut sizes[..last] {
                let mut size = 0u64;
                loop {
                    let byte = read_byte(reader)?;
                    size += u64::from(byte);
                    if byte != 0xFF {
                        break;
                    }
                }
                *slot = size as u32;
                known += size;
            }
        } else if lacing == LACING_EBML {
            let mut size = 0u64;
            if last != 0 {
                size = read_vint(reader)?;
            }
            for (i, slot) in sizes[..last].iter_mut().enumerate() {
                if i > 0 {
                    let delta = read_lace_delta(reader)?;
                    let next = (size as i64).wrapping_add(delta);
                    if next < 0 {
                        valid = false;
                    }
                    size = next.max(0) as u64;
                }
                if size > u64::from(u32::MAX) {
                    valid = false;
                }
                *slot = size as u32;
                known = known.wrapping_add(size);
            }
        }

        let payload = reader.tell();
        if payload > end {
            return None;
        }
        let total = (end - payload) as u64;
        if lacing == LACING_FIXED {
            if total % count as u64 != 0 {
                valid = false;
            }
            sizes[..count].fill((total / count as u64) as u32);
        } else if known > total {
            valid = false;
        } else {
            sizes[last] = (total - known) as u32;
        }
        Some(if valid { count as u16 } else { 0 })
    }

    fn group_references(&mut self, block_end: i64) -> bool {
        self.reader.seek(block_end);
        while self.reader.tell() < self.group_end {
            let Some((id, size)) = read_header(&self.reader) else {
                return false;
            };
            if size == UNKNOWN_SIZE {
                break;
            }
            if id == ID_REFERENCE_BLOCK {
                self.group_has_ref = true;
            }
            self.reader.skip(size as i64);
        }
        true
    }

    fn next_block(&mut self) -> Option<Block> {
        loop {
            if self.reader.tell() >= self.segment_end {
                return None;
            }
            let (id, size) = read_header(&self.reader)?;
            if id == ID_CLUSTER {
                self.cluster_time = 0;
                self.group_end = 0;
                continue;
            }
            if id == ID_BLOCK_GROUP {
                self.group_end = if size == UNKNOWN_SIZE { 0 } else { self.reader.tell().wrapping_add(size as i64) };
                self.group_has_ref = false;
                continue;
            }
            if size == UNKNOWN_SIZE {
                return None;
            }

            let end = self.reader.tell().wrapping_add(size as i64);
            if id == ID_REFERENCE_BLOCK {
                self.group_has_ref = true;
            }
            if id == ID_TIMESTAMP && size <= 8 {
                let mut bytes = [0; 8];
                if !self.reader.read_all(&mut bytes[..size as usize]) {
                    return None;
                }
                self.cluster_time = span_uint(&bytes[..size as usize]);
                continue;
            }
            if id != ID_SIMPLE_BLOCK && id != ID_BLOCK {
                self.reader.seek(end);
                continue;
            }

            let track = read_vint(&self.reader)?;
            let mut head = [0; 3];
            if !self.reader.read_all(&mut head) {
                return None;
            }
            if track != self.video_track && track != self.audio_track {
                self.reader.seek(end);
                continue;
            }
            let relative = i16::from_be_bytes([head[0], head[1]]);
            let flags = head[2];
            let lacing = flags & LACING_MASK;
            let laces = if lacing != 0 { self.read_lacing(lacing, end)? } else { 0 };
            let payload = self.reader.tell();
            if payload > end {
                return None;
            }

            let timestamp = (self.cluster_time as i64).wrapping_add(i64::from(relative)).max(0);
            let keyframe = if id == ID_SIMPLE_BLOCK {
                flags & 0x80 != 0
            } else if track == self.video_track && end <= self.group_end {
                if !self.group_references(end) {
                    return None;
                }
                self.reader.seek(payload);
                !self.group_has_ref
            } else {
                true
            };
            return Some(Block {
                track,
                pts_us: self.to_us(timestamp),
                keyframe,
                laced: lacing != 0,
                laces,
                end,
                payload: (end - payload) as u32,
            });
        }
    }

    fn probe_frame_interval(&mut self) -> i64 {
        self.reader.seek(self.first_cluster);
        self.cluster_time = 0;
        let mut first = None;
        for _ in 0..PROBE_BLOCKS {
            let Some(block) = self.next_block() else {
                break;
            };
            self.reader.seek(block.end);
            if block.track != self.video_track {
                continue;
            }
            match first {
                None => first = Some(block.pts_us),
                Some(first) if block.pts_us > first => return block.pts_us - first,
                Some(_) => {}
            }
        }
        0
    }

    fn parse_simple_tags(&mut self, mut span: &[u8], depth: usize) {
        if depth == MAX_TAG_DEPTH {
            return;
        }
        while let Some((id, body)) = span_next(&mut span) {
            if id != ID_SIMPLE_TAG {
                continue;
            }
            let (mut name, mut value): (&[u8], Option<&[u8]>) = (&[], None);
            let mut children = body;
            while let Some((child, child_body)) = span_next(&mut children) {
                if child == ID_TAG_NAME {
                    name = span_string(child_body, NAME_BYTES);
                }
                if child == ID_TAG_STRING {
                    value = Some(child_body);
                }
            }
            if let (Some(value), Some(field)) = (value, tag_field_of(name)) {
                self.info.tags.set(field, value, Encoding::Utf8);
            }
            self.parse_simple_tags(body, depth + 1);
        }
    }

    fn parse_tags(&mut self, mut span: &[u8]) {
        self.have_tags = true;
        while let Some((id, body)) = span_next(&mut span) {
            if id == ID_TAG {
                self.parse_simple_tags(body, 0);
            }
        }
    }

    fn parse_attachments(&mut self, mut span: &[u8]) {
        self.have_attachments = true;
        while let Some((id, mut body)) = span_next(&mut span) {
            if id != ID_ATTACHED_FILE {
                continue;
            }
            let (mut name, mut data): (&[u8], Option<&[u8]>) = (&[], None);
            while let Some((child, child_body)) = span_next(&mut body) {
                if child == ID_FILE_NAME {
                    name = span_string(child_body, NAME_BYTES);
                }
                if child == ID_FILE_DATA {
                    data = Some(child_body);
                }
            }
            if let Some(data) = data.filter(|_| self.want_cover) {
                let front = name.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"cover"));
                self.info.tags.set_cover(data, front);
            }
        }
    }

    // Tags and Attachments may follow the clusters, where the header walk stops.
    fn read_tag_element(&mut self, offset: i64, want: u32) {
        if offset <= 0 || offset >= self.segment_end {
            return;
        }
        self.reader.seek(offset);
        let limit = if want == ID_TAGS { MAX_TAGS_BYTES } else { MAX_ATTACHMENTS_BYTES };
        let Some((id, size)) = read_header(&self.reader) else {
            return;
        };
        if id != want {
            return;
        }
        let Some(data) = load_body(&self.reader, size, limit) else {
            return;
        };
        if want == ID_TAGS {
            self.parse_tags(&data);
        } else {
            self.parse_attachments(&data);
        }
    }

    fn read_deferred_tags(&mut self) {
        if !self.have_tags {
            self.read_tag_element(self.tags_offset, ID_TAGS);
        }
        if !self.have_attachments {
            self.read_tag_element(self.attachments_offset, ID_ATTACHMENTS);
        }
    }

    fn video_codec(&self) -> VideoCodec {
        self.info.video.as_ref().map_or(VideoCodec::Unsupported, |video| video.codec)
    }

    fn drop_laces(&mut self) {
        if self.lace_count != 0 {
            self.reader.seek(self.lace_end);
        }
        self.lace_count = 0;
        self.lace_next = 0;
    }

    fn step_lace(&mut self) {
        self.lace_next += 1;
        if self.lace_next == self.lace_count {
            self.drop_laces();
        }
    }

    fn read_lace(&mut self) -> Result<Option<Packet>> {
        let start = self.reader.tell();
        let size = self.lace_sizes[usize::from(self.lace_next)];
        if size == 0 || size as usize > self.max_view {
            self.reader.seek(start + i64::from(size));
            self.step_lace();
            return Ok(None);
        }

        let data = match self.reader.view(size as usize) {
            Ok(data) => data,
            Err(e) => {
                self.reader.seek(start);
                return Err(e);
            }
        };
        self.reader.seek(start + i64::from(size));
        self.step_lace();
        Ok(Some(Packet::audio(self.lace_pts_us, data)))
    }

    fn cue_at_or_before(&self, pts_us: i64) -> usize {
        let target_ms = pts_us / 1000;
        let (mut low, mut high) = (0, self.cues.len());
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if i64::from(self.cues[middle].time_ms) <= target_ms {
                low = middle;
            } else {
                high = middle;
            }
        }
        low
    }

    fn seek_to_video_at(&mut self, pts_us: i64) -> bool {
        let mut found = None;
        loop {
            let start = self.reader.tell();
            let (cluster_time, group_end) = (self.cluster_time, self.group_end);
            let Some(block) = self.next_block() else {
                break;
            };
            self.reader.seek(block.end);
            if block.track != self.video_track {
                continue;
            }
            if block.pts_us > pts_us {
                break;
            }
            found = Some((start, cluster_time, group_end, block.pts_us));
        }
        let Some((start, cluster_time, group_end, pts)) = found else {
            return false;
        };
        self.reader.seek(start);
        self.cluster_time = cluster_time;
        self.group_end = group_end;
        self.skip_before_us = pts;
        true
    }

    fn rewind_block_state(&mut self) {
        self.lace_count = 0;
        self.lace_next = 0;
        self.cluster_time = 0;
        self.group_end = 0;
    }

    /// Where the first video keyframe of the cue at or before `pts_us` sits in
    /// the file, as offset and size. Moves the read position: seek before
    /// reading on.
    pub fn keyframe_at(&mut self, pts_us: i64) -> Option<(u64, u32)> {
        if self.video_track == 0 {
            return None;
        }
        let mut start = self.first_cluster;
        if pts_us > 0 && !self.cues.is_empty() {
            start = self.segment_start + i64::from(self.cues[self.cue_at_or_before(pts_us)].position);
        }
        self.reader.seek(start);
        self.rewind_block_state();
        for _ in 0..KEYFRAME_SCAN_BLOCKS {
            let block = self.next_block()?;
            self.reader.seek(block.end);
            if block.track != self.video_track || !block.keyframe || block.laced || block.payload == 0 {
                continue;
            }
            return Some(((block.end - i64::from(block.payload)) as u64, block.payload));
        }
        None
    }

    pub fn keyframe_before(&self, pts_us: i64) -> Option<i64> {
        if self.cues.is_empty() || self.video_codec() == VideoCodec::Mjpeg {
            return None;
        }
        let key = i64::from(self.cues[self.cue_at_or_before(pts_us)].time_ms) * 1000;
        (key <= pts_us).then_some(key)
    }
}

fn parse_projection(track: &mut Track, mut span: &[u8]) {
    while let Some((id, body)) = span_next(&mut span) {
        match id {
            ID_PROJECTION_TYPE => track.projection_type = span_uint(body),
            ID_POSE_YAW => track.yaw = span_float(body),
            ID_POSE_PITCH => track.pitch = span_float(body),
            ID_POSE_ROLL => track.roll = span_float(body),
            _ => {}
        }
    }
}

fn parse_video(track: &mut Track, mut span: &[u8]) {
    while let Some((id, body)) = span_next(&mut span) {
        match id {
            ID_PIXEL_WIDTH => track.width = span_uint(body) as u32,
            ID_PIXEL_HEIGHT => track.height = span_uint(body) as u32,
            ID_PROJECTION => parse_projection(track, body),
            _ => {}
        }
    }
}

fn parse_audio(track: &mut Track, mut span: &[u8]) {
    while let Some((id, body)) = span_next(&mut span) {
        match id {
            ID_SAMPLING_FREQ => track.sample_rate = span_float(body),
            ID_CHANNELS => track.channels = span_uint(body),
            ID_BIT_DEPTH => track.bits = span_uint(body),
            _ => {}
        }
    }
}

fn use_wave_format(audio: &mut AudioTrack, wave: &[u8]) {
    if wave.len() < WAVE_FORMAT_BYTES {
        return;
    }
    let le16 = |at: usize| u16::from_le_bytes([wave[at], wave[at + 1]]);
    if le16(0) != WAVE_FORMAT_IMA_ADPCM {
        return;
    }
    audio.channels = le16(2) as u8;
    audio.sample_rate = u32::from_le_bytes([wave[4], wave[5], wave[6], wave[7]]);
    audio.block_align = le16(12);
    audio.bits_per_sample = le16(14) as u8;
    let extra = usize::from(le16(16)).min(wave.len() - WAVE_FORMAT_BYTES);
    audio.codec_private = copy_private(&wave[WAVE_FORMAT_BYTES..WAVE_FORMAT_BYTES + extra]);
    audio.codec = AudioCodec::AdpcmIma;
}

fn read_lace_delta(reader: &Buffer) -> Option<i64> {
    let byte = read_byte(reader)?;
    let length = vint_length(byte);
    if length > 8 {
        return None;
    }
    let mut value = i64::from(byte & (0xFFu32 >> length) as u8);
    for _ in 1..length {
        value = value << 8 | i64::from(read_byte(reader)?);
    }
    Some(value - ((1i64 << (7 * length - 1)) - 1))
}

fn tag_field_of(name: &[u8]) -> Option<Field> {
    match name {
        b"TITLE" => Some(Field::Title),
        b"ARTIST" => Some(Field::Artist),
        b"ALBUM" => Some(Field::Album),
        b"ALBUM_ARTIST" => Some(Field::AlbumArtist),
        b"PART_NUMBER" => Some(Field::Track),
        b"DATE_RELEASED" | b"DATE_RECORDED" => Some(Field::Date),
        _ => None,
    }
}

impl Demuxer for MkvDemuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        let audio_usable = self.info.audio_usable();
        loop {
            if self.lace_next < self.lace_count {
                if !self.want_audio {
                    self.drop_laces();
                    continue;
                }
                if let Some(packet) = self.read_lace()? {
                    return Ok(Some(packet));
                }
                continue;
            }

            let start = self.reader.tell();
            let cluster_time = self.cluster_time;
            let Some(block) = self.next_block() else {
                self.reader.check()?;
                return Ok(None);
            };

            let video = block.track == self.video_track;
            let skip = (if video { block.laced } else { !(self.want_audio && audio_usable) })
                || (block.laced && block.laces == 0)
                || block.payload == 0
                || (!block.laced && block.payload as usize > self.max_view)
                || (self.skip_before_us >= 0 && block.pts_us < self.skip_before_us)
                || (!video && self.audio_before_us >= 0 && block.pts_us < self.audio_before_us)
                || (video && self.need_keyframe && !block.keyframe);
            if skip {
                self.reader.seek(block.end);
                continue;
            }
            if !video {
                self.audio_before_us = -1;
            }
            if block.laced {
                self.lace_count = block.laces;
                self.lace_next = 0;
                self.lace_pts_us = block.pts_us;
                self.lace_end = block.end;
                continue;
            }

            let data = match self.reader.view(block.payload as usize) {
                Ok(data) => data,
                Err(e) => {
                    self.reader.seek(start);
                    self.cluster_time = cluster_time;
                    return Err(e);
                }
            };
            self.reader.seek(block.end);
            if video {
                self.skip_before_us = -1;
                self.need_keyframe = false;
            }
            let kind = if video { PacketKind::Video } else { PacketKind::Audio };
            return Ok(Some(Packet { kind, pts_us: block.pts_us, keyframe: block.keyframe, data, frame: 0 }));
        }
    }

    /// Needs `Cues`; without them only a seek to 0 works. MJPEG lands on the
    /// block on screen at the target and reports the target, H.264 on the cue's
    /// own time.
    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        if pts_us <= 0 {
            self.reader.seek(self.first_cluster);
            self.rewind_block_state();
            self.skip_before_us = -1;
            self.audio_before_us = -1;
            self.need_keyframe = false;
            return Ok(0);
        }
        if self.cues.is_empty() {
            return Err(Error::NotSeekable);
        }

        let cue = self.cues[self.cue_at_or_before(pts_us)];
        let position = self.segment_start + i64::from(cue.position);
        self.reader.seek(position);
        self.rewind_block_state();
        self.audio_before_us = -1;
        if self.video_codec() == VideoCodec::Mjpeg {
            if self.seek_to_video_at(pts_us) {
                self.audio_before_us = pts_us;
            } else {
                self.reader.seek(position);
                self.cluster_time = 0;
                self.group_end = 0;
                let interval = self.info.video.as_ref().map_or(0, |video| video.frame_interval_us);
                self.skip_before_us = pts_us - interval / 2;
            }
            self.need_keyframe = false;
            Ok(pts_us)
        } else {
            self.skip_before_us = i64::from(cue.time_ms) * 1000;
            self.need_keyframe = true;
            Ok(self.skip_before_us)
        }
    }

    fn set_want_audio(&mut self, want: bool) {
        self.want_audio = want;
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}
