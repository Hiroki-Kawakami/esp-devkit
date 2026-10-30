/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::CStr;

use devkit::ByteReader;
use idf::heap::{self, Caps};
use idf::log::{info, warn};

use crate::buffer::{Arena, Buffer, psram_first};
use crate::demux::{AudioTrack, Container, Demuxer, Info, OpenOptions, Packet, PacketKind, VideoCodec, VideoTrack};
use crate::error::{Error, Result};
use crate::id3;
use crate::riff::{self, CHUNK_HEADER_BYTES, Chunk, fourcc, read_fourcc};
use crate::tags::Cover;

const TAG: &CStr = c"media_avi";

const MAX_STREAMS: usize = 8;
const MAX_INDEX_ENTRIES: u32 = 60000;
const KEYFRAME_SCAN_CHUNKS: usize = 256;
const BITMAP_EXTRA_LIMIT: u32 = 4096;
const INDEX_KEYFRAME: u32 = 0x10;
const DEFAULT_FRAME_INTERVAL_US: u32 = 100_000;

const MAIN_HEADER_BYTES: usize = 56;
const STREAM_HEADER_BYTES: usize = 56;
const BITMAP_INFO_BYTES: usize = 40;
const INDEX_ENTRY_BYTES: usize = 16;

const ID_HDRL: u32 = fourcc(b"hdrl");
const ID_STRL: u32 = fourcc(b"strl");
const ID_MOVI: u32 = fourcc(b"movi");
const ID_AVIH: u32 = fourcc(b"avih");
const ID_STRH: u32 = fourcc(b"strh");
const ID_STRF: u32 = fourcc(b"strf");
const ID_IDX1: u32 = fourcc(b"idx1");
const ID_VIDS: u32 = fourcc(b"vids");
const ID_AUDS: u32 = fourcc(b"auds");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    None,
    Video,
    Audio,
}

#[derive(Clone, Copy)]
struct SeekPoint {
    frame: u32,
    offset: u32,
}

fn video_codec_of(compression: u32) -> VideoCodec {
    match &compression.to_le_bytes() {
        b"MJPG" | b"mjpg" | b"jpeg" | b"JPEG" => VideoCodec::Mjpeg,
        b"H264" | b"h264" | b"X264" | b"x264" | b"AVC1" | b"avc1" | b"DAVC" => VideoCodec::H264,
        b"mpg2" | b"MPG2" | b"MPEG" => VideoCodec::Mpeg2,
        _ => VideoCodec::Unsupported,
    }
}

fn video_codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Mjpeg => "MJPEG",
        VideoCodec::H264 => "H.264",
        VideoCodec::Mpeg2 => "MPEG-2",
        VideoCodec::Unsupported => "unsupported",
    }
}

fn starts_idr(data: &[u8], length_size: u8) -> bool {
    let size = data.len();
    let length_bytes = usize::from(length_size);
    let mut pos = 0;
    while pos < size {
        let nal;
        if length_bytes != 0 {
            if size - pos <= length_bytes {
                return false;
            }
            let nal_size = data[pos..pos + length_bytes].iter().fold(0usize, |v, &b| v.wrapping_shl(8) | usize::from(b)) & 0xFFFF_FFFF;
            nal = pos + length_bytes;
            pos = if nal_size > size - nal { size } else { nal + nal_size };
            if nal_size == 0 {
                continue;
            }
        } else {
            while pos + 3 < size && !(data[pos] == 0 && data[pos + 1] == 0 && data[pos + 2] == 1) {
                pos += 1;
            }
            if pos + 3 >= size {
                return false;
            }
            nal = pos + 3;
            pos = nal;
        }
        let kind = data[nal] & 0x1F;
        if (1..=5).contains(&kind) {
            return kind == 5;
        }
    }
    false
}

fn starts_intra_picture(data: &[u8]) -> bool {
    data.windows(6)
        .find(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] == 0)
        .is_some_and(|w| (w[5] >> 3) & 7 == 1)
}

/// RIFF AVI. The API is frame-numbered, since AVI has no timestamps; the
/// `Demuxer` times are the frame number scaled by the frame interval.
pub struct AviDemuxer {
    reader: Buffer,
    info: Info,
    max_view: usize,
    movi_start: i64,
    movi_end: i64,
    idx1_offset: i64,
    idx1_size: u32,
    index_base: i64,
    points: Option<Vec<SeekPoint, Caps>>,
    key_flags: Option<Box<[u8], Caps>>,
    key_flag_count: u32,
    next_frame: u32,
    streams: [Stream; MAX_STREAMS],
    stream_count: usize,
    nal_length_size: u8,
    want_audio: bool,
}

#[derive(Default)]
struct Parsed {
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
}

impl AviDemuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let max_view = reader.max_view();
        let mut demux = Self {
            reader,
            info: Info::new(Container::Avi),
            max_view,
            movi_start: 0,
            movi_end: 0,
            idx1_offset: 0,
            idx1_size: 0,
            index_base: 0,
            points: None,
            key_flags: None,
            key_flag_count: 0,
            next_frame: 0,
            streams: [Stream::None; MAX_STREAMS],
            stream_count: 0,
            nal_length_size: 0,
            want_audio: true,
        };
        demux.info.tags.cover_scanned = true;
        demux.info.seekable = false;

        let is_avi = match (Chunk::read(&demux.reader), read_fourcc(&demux.reader)) {
            (Some(chunk), Some(kind)) => chunk.fourcc == riff::ID_RIFF && kind == fourcc(b"AVI "),
            _ => false,
        };
        if !is_avi {
            return Err(Error::Format("not an AVI file"));
        }

        let mut parsed = Parsed::default();
        demux.walk_file(&mut parsed, options);
        if demux.movi_start == 0 || demux.movi_end <= demux.movi_start {
            return Err(Error::Format("no movie data in this AVI"));
        }
        let mut video = match parsed.video {
            Some(track) if track.codec != VideoCodec::Unsupported => track,
            _ => return Err(Error::Format("not an MJPEG, H.264 or MPEG-2 AVI")),
        };
        let mut audio = parsed.audio;

        demux.build_index(&mut video, &mut audio);

        if video.max_frame_bytes as usize > max_view {
            return Err(Error::Format("video frames are too large to buffer"));
        }
        if video.frame_interval_us == 0 {
            video.frame_interval_us = i64::from(DEFAULT_FRAME_INTERVAL_US);
        }
        if video.frame_count == 0 {
            return Err(Error::Format("this AVI declares no frames"));
        }

        info!(TAG, "{}: {} {}x{}, {} frames, {} us/frame, video <= {} B", path.to_str().unwrap_or("?"),
              video_codec_name(video.codec), video.width, video.height, video.frame_count,
              video.frame_interval_us, video.max_frame_bytes);

        demux.info.duration_us = i64::from(video.frame_count) * video.frame_interval_us;
        demux.info.video = Some(video);
        demux.info.audio = audio;
        demux.reader.seek(demux.movi_start);
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn walk_file(&mut self, parsed: &mut Parsed, options: &OpenOptions) {
        let file_end = self.reader.size();
        while self.reader.tell() + CHUNK_HEADER_BYTES <= file_end {
            let Some(chunk) = Chunk::read(&self.reader) else {
                break;
            };
            let body = self.reader.tell();
            let next = (body + chunk.padded()).min(file_end);

            if chunk.fourcc == riff::ID_LIST {
                let Some(list) = read_fourcc(&self.reader) else {
                    break;
                };
                if list == ID_HDRL {
                    self.parse_header_list(parsed, next);
                } else if list == ID_MOVI {
                    self.movi_start = self.reader.tell();
                    self.movi_end = next;
                } else if list == riff::ID_INFO {
                    riff::read_info(&self.reader, next, &mut self.info.tags);
                }
            } else if chunk.fourcc == ID_IDX1 {
                self.idx1_offset = body;
                self.idx1_size = chunk.size;
            } else if chunk.fourcc == riff::ID_ID3_LOWER || chunk.fourcc == riff::ID_ID3 {
                id3::read_v2(&self.reader, body, &mut self.info.tags, options.want_cover);
            }
            self.reader.seek(next);
        }
    }

    fn parse_header_list(&mut self, parsed: &mut Parsed, list_end: i64) {
        while self.reader.tell() + CHUNK_HEADER_BYTES <= list_end {
            let Some(chunk) = Chunk::read(&self.reader) else {
                return;
            };
            let next = self.reader.tell() + chunk.padded();

            if chunk.fourcc == ID_AVIH && chunk.size as usize >= MAIN_HEADER_BYTES {
                let mut header = [0; MAIN_HEADER_BYTES];
                if !self.reader.read_all(&mut header) {
                    return;
                }
                let track = video_slot(parsed);
                let word = |at: usize| ByteReader::new(&header).at(at).and_then(|mut r| r.u32_le()).unwrap_or(0);
                track.frame_interval_us = i64::from(word(0));
                track.frame_count = word(16);
                track.width = word(32);
                track.height = word(36);
            } else if chunk.fourcc == riff::ID_LIST {
                match read_fourcc(&self.reader) {
                    None => return,
                    Some(ID_STRL) => self.parse_stream(parsed, next),
                    Some(_) => {}
                }
            }
            self.reader.seek(next);
        }
    }

    fn parse_stream(&mut self, parsed: &mut Parsed, list_end: i64) {
        let mut strh = None;
        let index = (self.stream_count < MAX_STREAMS).then_some(self.stream_count);

        while self.reader.tell() + CHUNK_HEADER_BYTES <= list_end {
            let Some(chunk) = Chunk::read(&self.reader) else {
                return;
            };
            let next = self.reader.tell() + chunk.padded();

            if chunk.fourcc == ID_STRH && chunk.size as usize >= STREAM_HEADER_BYTES {
                let mut header = [0; STREAM_HEADER_BYTES];
                if !self.reader.read_all(&mut header) {
                    return;
                }
                strh = Some(header);
            } else if let (ID_STRF, Some(header), Some(index)) = (chunk.fourcc, &strh, index) {
                let word = |at: usize| ByteReader::new(header).at(at).and_then(|mut r| r.u32_le()).unwrap_or(0);
                let kind = word(0);
                if kind == ID_VIDS && chunk.size as usize >= BITMAP_INFO_BYTES {
                    let (scale, rate, length) = (word(20), word(24), word(32));
                    let mut bitmap = [0; BITMAP_INFO_BYTES];
                    if !self.reader.read_all(&mut bitmap) {
                        return;
                    }
                    let field = |at: usize| ByteReader::new(&bitmap).at(at).and_then(|mut r| r.u32_le()).unwrap_or(0);
                    self.streams[index] = Stream::Video;
                    let track = video_slot(parsed);
                    track.codec = video_codec_of(field(16));
                    if field(4) != 0 {
                        track.width = field(4);
                    }
                    if field(8) != 0 {
                        track.height = field(8);
                    }
                    if rate != 0 && scale != 0 {
                        track.frame_interval_us = i64::from((u64::from(scale) * 1_000_000 / u64::from(rate)) as u32);
                    }
                    if length != 0 {
                        track.frame_count = length;
                    }
                    if matches!(track.codec, VideoCodec::H264 | VideoCodec::Mpeg2) {
                        let mut extra = chunk.size - BITMAP_INFO_BYTES as u32;
                        let declared = field(0);
                        if declared > BITMAP_INFO_BYTES as u32 && declared - (BITMAP_INFO_BYTES as u32) < extra {
                            extra = declared - BITMAP_INFO_BYTES as u32;
                        }
                        self.read_bitmap_extra(parsed, extra);
                    }
                } else if kind == ID_AUDS {
                    let Some(format) = riff::read_wave_format(&self.reader, chunk.size) else {
                        return;
                    };
                    self.streams[index] = Stream::Audio;
                    parsed.audio = Some(AudioTrack {
                        codec: format.codec,
                        sample_rate: format.sample_rate,
                        bitrate_bps: format.bitrate_bps,
                        channels: format.channels,
                        bits_per_sample: format.bits_per_sample,
                        block_align: format.block_align,
                        max_frame_bytes: 0,
                        codec_private: format.extra,
                    });
                }
            }
            self.reader.seek(next);
        }

        if index.is_some() {
            self.stream_count += 1;
        }
    }

    fn read_bitmap_extra(&mut self, parsed: &mut Parsed, available: u32) {
        let track = video_slot(parsed);
        track.codec_private = Box::default();
        self.nal_length_size = 0;
        if available == 0 || available > BITMAP_EXTRA_LIMIT {
            return;
        }
        let mut extra = vec![0; available as usize].into_boxed_slice();
        if !self.reader.read_all(&mut extra) {
            return;
        }
        if extra.len() >= 7 && extra[0] == 1 && extra[4] & 0x03 != 2 {
            self.nal_length_size = (extra[4] & 0x03) + 1;
        }
        track.codec_private = extra;
    }

    fn stream_of(&self, id: u32) -> Option<Stream> {
        let id = id.to_le_bytes();
        if !id[0].is_ascii_digit() || !id[1].is_ascii_digit() {
            return None;
        }
        let index = usize::from(id[0] - b'0') * 10 + usize::from(id[1] - b'0');
        if index >= self.stream_count {
            return None;
        }
        let declared = self.streams[index];
        let video_suffix = id[2] == b'd' && (id[3] == b'b' || id[3] == b'c');
        let audio_suffix = id[2] == b'w' && id[3] == b'b';
        match declared {
            Stream::Video if video_suffix => Some(Stream::Video),
            Stream::Audio if audio_suffix => Some(Stream::Audio),
            _ => None,
        }
    }

    fn index_base_valid(&self, base: i64, first_offset: u32) -> bool {
        let position = base + i64::from(first_offset);
        if position < 0 || position + CHUNK_HEADER_BYTES > self.reader.size() {
            return false;
        }
        self.reader.seek(position);
        Chunk::read(&self.reader).is_some_and(|chunk| self.stream_of(chunk.fourcc).is_some())
    }

    fn build_index(&mut self, video: &mut VideoTrack, audio: &mut Option<AudioTrack>) {
        if self.idx1_size == 0 {
            return;
        }
        let entries = self.idx1_size / INDEX_ENTRY_BYTES as u32;
        if entries == 0 {
            return;
        }

        let all_key = video.codec == VideoCodec::Mjpeg;
        let capacity = entries.min(MAX_INDEX_ENTRIES) as usize;
        let mut points = psram_first(|caps| heap::try_vec::<SeekPoint>(capacity, caps)).ok();
        let mut flags = if all_key {
            None
        } else {
            psram_first(|caps| heap::zeroed_slice::<u8>((entries as usize).div_ceil(8), caps)).ok()
        };
        if points.is_none() || (!all_key && flags.is_none()) {
            warn!(TAG, "no memory for the frame index; seeking disabled");
            points = None;
            flags = None;
        }

        let (mut video_frames, mut keyframes, mut step, mut max_video, mut max_audio) = (0u32, 0u32, 1u32, 0, 0);
        let mut first_video_offset = None;

        self.reader.set_readahead(true);
        self.reader.seek(self.idx1_offset);
        for _ in 0..entries {
            let mut entry = [0; INDEX_ENTRY_BYTES];
            if !self.reader.read_all(&mut entry) {
                break;
            }
            let word = |at: usize| u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]]);
            let (entry_flags, offset, size) = (word(4), word(8), word(12));
            match self.stream_of(word(0)) {
                None => continue,
                Some(Stream::Video) => {}
                Some(_) => {
                    max_audio = max_audio.max(size);
                    continue;
                }
            }
            first_video_offset.get_or_insert(offset);
            max_video = max_video.max(size);
            let frame = video_frames;
            video_frames += 1;
            let Some(stored) = points.as_mut() else {
                continue;
            };
            if !all_key && entry_flags & INDEX_KEYFRAME == 0 {
                continue;
            }
            if let Some(flags) = flags.as_mut() {
                flags[(frame >> 3) as usize] |= 1 << (frame & 7);
            }
            let key = keyframes;
            keyframes += 1;
            if key % step != 0 {
                continue;
            }
            if stored.len() == capacity {
                let kept = stored.len() / 2;
                for k in 0..kept {
                    stored[k] = stored[k * 2];
                }
                stored.truncate(kept);
                step *= 2;
                if key % step != 0 {
                    continue;
                }
            }
            stored.push(SeekPoint { frame, offset });
        }

        if max_video != 0 {
            video.max_frame_bytes = max_video;
        }
        if max_audio != 0 {
            if let Some(audio) = audio {
                audio.max_frame_bytes = max_audio;
            }
        }
        if video_frames != 0 {
            video.frame_count = video_frames;
        }

        let Some(first) = first_video_offset.filter(|_| keyframes != 0) else {
            return;
        };
        self.key_flag_count = if flags.is_some() { video_frames } else { 0 };
        self.key_flags = flags;

        let mut base = self.movi_start - 4;
        if !self.index_base_valid(base, first) {
            base = 0;
            if !self.index_base_valid(base, first) {
                return;
            }
        }
        self.index_base = base;
        self.points = points;
        self.info.seekable = true;
    }

    fn keyframe_of(&self, frame: u32, data: &[u8]) -> bool {
        let codec = self.video_codec();
        if codec == VideoCodec::Mjpeg {
            return true;
        }
        if frame < self.key_flag_count {
            if let Some(flags) = &self.key_flags {
                return (flags[(frame >> 3) as usize] >> (frame & 7)) & 1 != 0;
            }
        }
        if codec == VideoCodec::Mpeg2 {
            return starts_intra_picture(data);
        }
        starts_idr(data, self.nal_length_size)
    }

    fn video_codec(&self) -> VideoCodec {
        self.info.video.as_ref().map_or(VideoCodec::Unsupported, |video| video.codec)
    }

    fn frame_count(&self) -> u32 {
        self.info.video.as_ref().map_or(0, |video| video.frame_count)
    }

    fn frame_interval_us(&self) -> i64 {
        self.info.video.as_ref().map_or(0, |video| video.frame_interval_us)
    }

    fn point_at_or_before(&self, frame: u32) -> Option<SeekPoint> {
        let points = self.points.as_ref()?;
        let (mut low, mut high) = (0, points.len());
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if points[middle].frame <= frame {
                low = middle;
            } else {
                high = middle;
            }
        }
        points.get(low).copied()
    }

    /// Lands on the last keyframe at or before `frame`. Without `idx1` only
    /// frame 0 can be sought.
    pub fn seek_frame(&mut self, frame: u32) -> Result<u32> {
        let mut landed = 0;
        let mut position = self.movi_start;
        if frame > 0 {
            if self.points.is_none() || frame >= self.frame_count() {
                return Err(Error::NotSeekable);
            }
            if let Some(point) = self.point_at_or_before(frame).filter(|point| point.frame <= frame) {
                landed = point.frame;
                position = self.index_base + i64::from(point.offset);
            }
        }
        self.reader.seek(position);
        self.next_frame = landed;
        Ok(landed)
    }

    /// Where the last video keyframe at or before `frame` sits in the file, as
    /// offset and size. Moves the read position: seek before reading on.
    pub fn keyframe_at(&mut self, frame: u32) -> Option<(u64, u32)> {
        let mut position = self.movi_start;
        if frame > 0 {
            if let Some(point) = self.point_at_or_before(frame).filter(|point| point.frame <= frame) {
                position = self.index_base + i64::from(point.offset);
            }
        }
        self.reader.seek(position);
        for _ in 0..KEYFRAME_SCAN_CHUNKS {
            if self.reader.tell() + CHUNK_HEADER_BYTES > self.movi_end {
                return None;
            }
            let chunk = Chunk::read(&self.reader)?;
            let body = self.reader.tell();
            let kind = self.stream_of(chunk.fourcc);
            if kind.is_none() && chunk.fourcc == riff::ID_LIST {
                read_fourcc(&self.reader)?;
                continue;
            }
            if kind == Some(Stream::Video) && chunk.size != 0 && body + i64::from(chunk.size) <= self.movi_end {
                return Some((body as u64, chunk.size));
            }
            self.reader.seek(body + chunk.padded());
        }
        None
    }

    pub fn keyframe_before(&self, frame: u32) -> Option<u32> {
        if self.video_codec() == VideoCodec::Mjpeg {
            return None;
        }
        self.point_at_or_before(frame).map(|point| point.frame).filter(|&key| key <= frame)
    }
}

fn video_slot(parsed: &mut Parsed) -> &mut VideoTrack {
    parsed.video.get_or_insert_with(|| VideoTrack {
        codec: VideoCodec::Unsupported,
        width: 0,
        height: 0,
        frame_interval_us: 0,
        rotation_ccw: 0,
        frame_count: 0,
        max_frame_bytes: 0,
        codec_private: Box::default(),
    })
}

impl Demuxer for AviDemuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        while self.reader.tell() + CHUNK_HEADER_BYTES <= self.movi_end {
            let start = self.reader.tell();
            let Some(chunk) = Chunk::read(&self.reader) else {
                self.reader.check()?;
                return Ok(None);
            };
            let next = self.reader.tell() + chunk.padded();

            let Some(kind) = self.stream_of(chunk.fourcc) else {
                if chunk.fourcc == riff::ID_LIST {
                    if read_fourcc(&self.reader).is_none() {
                        self.reader.check()?;
                        return Ok(None);
                    }
                    continue;
                }
                self.reader.seek(next);
                continue;
            };

            let video = kind == Stream::Video;
            if (!video && !self.want_audio) || chunk.size == 0 || chunk.size as usize > self.max_view {
                if video {
                    self.next_frame = self.next_frame.wrapping_add(1);
                }
                self.reader.seek(next);
                continue;
            }

            let data = match self.reader.view(chunk.size as usize) {
                Ok(data) => data,
                Err(e) => {
                    self.reader.seek(start);
                    return Err(e);
                }
            };
            self.reader.seek(next);

            let interval = self.frame_interval_us();
            if !video {
                return Ok(Some(Packet::audio(i64::from(self.next_frame) * interval, data)));
            }
            let frame = self.next_frame;
            self.next_frame = self.next_frame.wrapping_add(1);
            let keyframe = self.keyframe_of(frame, &data);
            return Ok(Some(Packet { kind: PacketKind::Video, pts_us: i64::from(frame) * interval, keyframe, data, frame }));
        }
        Ok(None)
    }

    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        let interval = self.frame_interval_us();
        let frame = if pts_us > 0 && interval > 0 { (pts_us / interval) as u32 } else { 0 };
        Ok(i64::from(self.seek_frame(frame)?) * interval)
    }

    fn set_want_audio(&mut self, want: bool) {
        self.want_audio = want;
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}

