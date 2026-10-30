/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;

use idf::fs::File;

use crate::avi::AviDemuxer;
use crate::buffer::{Arena, Buffer, Interrupter, View};
use crate::error::{Error, Result};
use crate::es::EsDemuxer;
use crate::flac::FlacDemuxer;
use crate::mkv::MkvDemuxer;
use crate::mp4::Mp4Demuxer;
use crate::ogg::OggDemuxer;
use crate::tags::{Cover, Tags};
use crate::wav::WavDemuxer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Avi,
    Wav,
    Es,
    Flac,
    Ogg,
    Mkv,
    Mp4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    Mjpeg,
    H264,
    Mpeg2,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Pcm,
    Mp3,
    Mp2,
    AdpcmIma,
    Aac,
    Opus,
    Flac,
    Alac,
    Unsupported,
}

impl AudioCodec {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pcm => "PCM",
            Self::Mp3 => "MP3",
            Self::Mp2 => "MP2",
            Self::AdpcmIma => "IMA ADPCM",
            Self::Aac => "AAC",
            Self::Opus => "Opus",
            Self::Flac => "FLAC",
            Self::Alac => "ALAC",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrack {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub frame_interval_us: i64,
    pub rotation_ccw: u16,
    /// 0 where the container does not say.
    pub frame_count: u32,
    /// 0 where the container does not say.
    pub max_frame_bytes: u32,
    pub codec_private: Box<[u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub bitrate_bps: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub block_align: u16,
    /// 0 where the container does not say.
    pub max_frame_bytes: u32,
    /// FLAC's STREAMINFO body, Opus's OpusHead, ALAC's ALACSpecificConfig,
    /// AAC's AudioSpecificConfig, a WAVEFORMATEX's extra bytes.
    pub codec_private: Box<[u8]>,
}

pub struct Info {
    pub container: Container,
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
    pub duration_us: i64,
    /// `duration_us` comes from a count in the file rather than an estimate.
    pub duration_exact: bool,
    pub seekable: bool,
    pub tags: Box<Tags>,
}

impl Info {
    pub(crate) fn new(container: Container) -> Self {
        Self {
            container,
            video: None,
            audio: None,
            duration_us: 0,
            duration_exact: true,
            seekable: true,
            tags: Box::default(),
        }
    }

    pub(crate) fn audio_usable(&self) -> bool {
        self.audio.as_ref().is_some_and(|audio| audio.codec != AudioCodec::Unsupported)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKind {
    Video,
    Audio,
}

pub struct Packet {
    pub kind: PacketKind,
    pub pts_us: i64,
    pub keyframe: bool,
    pub data: View,
    /// The frame number of an AVI video packet, whose time derives from it;
    /// 0 for anything else.
    pub frame: u32,
}

impl Packet {
    pub(crate) fn audio(pts_us: i64, data: View) -> Self {
        Self { kind: PacketKind::Audio, pts_us, keyframe: true, data, frame: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenOptions {
    /// False skips reading any embedded picture; `Tags::cover_at` still says
    /// where it is.
    pub want_cover: bool,
}

pub trait Demuxer {
    fn info(&self) -> &Info;
    fn take_cover(&mut self) -> Option<Cover>;
    /// The next packet in reading order, `None` at the end. After `Interrupted`
    /// the demuxer stands on the element it started on.
    fn read(&mut self) -> Result<Option<Packet>>;
    /// Returns the time it landed on.
    fn seek(&mut self, pts_us: i64) -> Result<i64>;
    /// False leaves audio packets out. Starts true.
    fn set_want_audio(&mut self, _want: bool) {}
    fn buffer(&self) -> &Buffer;
    fn interrupter(&self) -> Interrupter {
        self.buffer().interrupter()
    }
}

pub enum Demux {
    Avi(Box<AviDemuxer>),
    Wav(Box<WavDemuxer>),
    Es(Box<EsDemuxer>),
    Flac(Box<FlacDemuxer>),
    Ogg(Box<OggDemuxer>),
    Mkv(Box<MkvDemuxer>),
    Mp4(Box<Mp4Demuxer>),
}

macro_rules! forward {
    ($self:ident, $d:ident => $call:expr) => {
        match $self {
            Demux::Avi($d) => $call,
            Demux::Wav($d) => $call,
            Demux::Es($d) => $call,
            Demux::Flac($d) => $call,
            Demux::Ogg($d) => $call,
            Demux::Mkv($d) => $call,
            Demux::Mp4($d) => $call,
        }
    };
}

impl Demuxer for Demux {
    fn info(&self) -> &Info {
        forward!(self, d => d.info())
    }

    fn take_cover(&mut self) -> Option<Cover> {
        forward!(self, d => d.take_cover())
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        forward!(self, d => d.read())
    }

    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        forward!(self, d => d.seek(pts_us))
    }

    fn set_want_audio(&mut self, want: bool) {
        forward!(self, d => d.set_want_audio(want))
    }

    fn buffer(&self) -> &Buffer {
        forward!(self, d => d.buffer())
    }
}

const SNIFF_BYTES: usize = 16;
const ID3_HEADER_BYTES: usize = 10;

fn looks_like_mp4(head: &[u8]) -> bool {
    const TOP_LEVEL: [&[u8; 4]; 7] = [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot"];
    head.len() >= 8 && TOP_LEVEL.iter().any(|name| head[4..8] == **name)
}

fn sniff(file: &File) -> Result<Container> {
    let mut head = [0; SNIFF_BYTES];
    let got = file.read(&mut head).map_err(|_| Error::Open)?;
    let head = &head[..got];
    if head.starts_with(b"RIFF") {
        return Ok(if head[8..].starts_with(b"WAVE") { Container::Wav } else { Container::Avi });
    }
    if head.starts_with(b"OggS") {
        return Ok(Container::Ogg);
    }
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Ok(Container::Mkv);
    }
    if head.starts_with(b"fLaC") {
        return Ok(Container::Flac);
    }
    if head.len() >= ID3_HEADER_BYTES && head.starts_with(b"ID3") {
        let size = head[6..10].iter().fold(0u64, |v, &b| v << 7 | u64::from(b & 0x7F));
        let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
        let mut magic = [0; 4];
        if file.seek(ID3_HEADER_BYTES as u64 + size + footer).is_ok() && file.read(&mut magic) == Ok(4) && magic == *b"fLaC" {
            return Ok(Container::Flac);
        }
        return Ok(Container::Es);
    }
    Ok(if looks_like_mp4(head) { Container::Mp4 } else { Container::Es })
}

/// Opens `path` with the demuxer its first bytes call for; anything else is
/// tried as an elementary stream.
pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Demux> {
    let container = sniff(&File::open(path).map_err(|_| Error::Open)?)?;
    let (a, o) = (arena, options);
    Ok(match container {
        Container::Avi => Demux::Avi(boxed(|| AviDemuxer::open(path, a, o))?),
        Container::Wav => Demux::Wav(boxed(|| WavDemuxer::open(path, a, o))?),
        Container::Es => Demux::Es(boxed(|| EsDemuxer::open(path, a, o))?),
        Container::Flac => Demux::Flac(boxed(|| FlacDemuxer::open(path, a, o))?),
        Container::Ogg => Demux::Ogg(boxed(|| OggDemuxer::open(path, a, o))?),
        Container::Mkv => Demux::Mkv(boxed(|| MkvDemuxer::open(path, a, o))?),
        Container::Mp4 => Demux::Mp4(boxed(|| Mp4Demuxer::open(path, a, o))?),
    })
}

// Not inlined: `open` would otherwise carry every demuxer on its stack frame.
#[inline(never)]
fn boxed<T>(open: impl FnOnce() -> Result<T>) -> Result<Box<T>> {
    Box::try_new(open()?).map_err(|_| Error::Memory)
}
