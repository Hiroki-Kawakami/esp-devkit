/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::info;

use crate::buffer::{Arena, Buffer};
use crate::demux::{AudioCodec, AudioTrack, Container, Demuxer, Info, OpenOptions, Packet};
use crate::error::{Error, Result};
use crate::riff::{self, AudioFormat, Chunk};
use crate::tags::Cover;
use crate::{id3, riff::read_fourcc};

const TAG: &CStr = c"media_wav";
const PACKET_BYTES: u32 = 32 * 1024;

/// One `data` range, cut into packets of about 32 KB of whole frames or whole
/// IMA ADPCM blocks.
pub struct WavDemuxer {
    reader: Buffer,
    info: Info,
    sample_rate: u32,
    data_start: i64,
    data_end: i64,
    cursor: i64,
    unit_bytes: u32,
    unit_samples: u32,
}

fn describe_format(format: &AudioFormat) -> Result<(u32, u32)> {
    if format.sample_rate == 0 || format.channels == 0 {
        return Err(Error::Format("this WAV has no audio format"));
    }
    match format.codec {
        AudioCodec::Pcm => {
            if !matches!(format.bits_per_sample, 16 | 24 | 32) {
                return Err(Error::Format("only 16, 24 and 32-bit PCM WAV files are supported"));
            }
            Ok((u32::from(format.channels) * u32::from(format.bits_per_sample / 8), 1))
        }
        AudioCodec::AdpcmIma => {
            let block = u32::from(format.block_align);
            let channels = u32::from(format.channels);
            if block == 0 || block % (4 * channels) != 0 {
                return Err(Error::Format("unusable IMA ADPCM block size"));
            }
            Ok((block, (block / channels - 4) * 2 + 1))
        }
        _ => Err(Error::Format("unsupported audio codec in this WAV")),
    }
}

impl WavDemuxer {
    pub fn open(path: &CStr, arena: Arena, options: &OpenOptions) -> Result<Self> {
        let reader = Buffer::open(path, arena)?;
        let mut info = Info::new(Container::Wav);
        info.tags.cover_scanned = true;

        let is_wav = match (Chunk::read(&reader), read_fourcc(&reader)) {
            (Some(chunk), Some(kind)) => chunk.fourcc == riff::ID_RIFF && kind == riff::fourcc(b"WAVE"),
            _ => false,
        };
        if !is_wav {
            return Err(Error::Format("not a WAV file"));
        }

        let file_end = reader.size();
        let mut format = None;
        let (mut data_start, mut data_end) = (0, 0);
        while let Some((chunk, body, mut next)) = riff::next_chunk(&reader, file_end) {
            if chunk.fourcc == riff::ID_FMT && format.is_none() {
                format = riff::read_wave_format(&reader, chunk.size);
            } else if chunk.fourcc == riff::ID_DATA && data_start == 0 {
                data_start = body;
                data_end = body + i64::from(chunk.size);
                if chunk.size == 0 || data_end > file_end {
                    data_end = file_end;
                }
                next = data_end + (data_end & 1);
            } else if chunk.fourcc == riff::ID_LIST {
                if read_fourcc(&reader) == Some(riff::ID_INFO) {
                    riff::read_info(&reader, next, &mut info.tags);
                }
            } else if chunk.fourcc == riff::ID_ID3_LOWER || chunk.fourcc == riff::ID_ID3 {
                id3::read_v2(&reader, body, &mut info.tags, options.want_cover);
            }
            if next > file_end {
                break;
            }
            reader.seek(next);
        }

        let Some(format) = format else {
            return Err(Error::Format("no format in this WAV"));
        };
        if data_start == 0 || data_end <= data_start {
            return Err(Error::Format("no audio data in this WAV"));
        }
        let (unit_bytes, unit_samples) = describe_format(&format)?;
        let bitrate_bps = if format.bitrate_bps != 0 {
            format.bitrate_bps
        } else {
            (format.sample_rate / unit_samples).wrapping_mul(unit_bytes).wrapping_mul(8)
        };

        let mut demux = Self {
            reader,
            sample_rate: format.sample_rate,
            data_start,
            data_end,
            cursor: data_start,
            unit_bytes,
            unit_samples,
            info,
        };
        let units = (data_end - data_start) as u64 / u64::from(unit_bytes);
        demux.info.duration_us = demux.units_to_us(units);
        demux.info.audio = Some(AudioTrack {
            codec: format.codec,
            sample_rate: format.sample_rate,
            bitrate_bps,
            channels: format.channels,
            bits_per_sample: format.bits_per_sample,
            block_align: format.block_align,
            max_frame_bytes: 0,
            codec_private: format.extra,
        });

        info!(TAG, "{}: {} {} Hz {} bit x{}, {} us", path.to_str().unwrap_or("?"), format.codec.name(),
              format.sample_rate, format.bits_per_sample, format.channels, demux.info.duration_us);

        demux.reader.seek(demux.cursor);
        demux.reader.set_readahead(true);
        Ok(demux)
    }

    fn units_to_us(&self, units: u64) -> i64 {
        (units.wrapping_mul(u64::from(self.unit_samples)).wrapping_mul(1_000_000) / u64::from(self.sample_rate)) as i64
    }

    fn us_to_units(&self, us: i64) -> u64 {
        if us <= 0 {
            return 0;
        }
        (us as u64).wrapping_mul(u64::from(self.sample_rate)) / 1_000_000 / u64::from(self.unit_samples)
    }
}

impl Demuxer for WavDemuxer {
    fn info(&self) -> &Info {
        &self.info
    }

    fn take_cover(&mut self) -> Option<Cover> {
        self.info.tags.cover.take()
    }

    fn read(&mut self) -> Result<Option<Packet>> {
        if self.cursor >= self.data_end {
            return Ok(None);
        }
        let left = (self.data_end - self.cursor) as u64 / u64::from(self.unit_bytes);
        if left == 0 {
            return Ok(None);
        }
        let units = (PACKET_BYTES / self.unit_bytes).max(1).min(left.min(u64::from(u32::MAX)) as u32);
        let size = units * self.unit_bytes;

        self.reader.seek(self.cursor);
        let data = self.reader.view(size as usize)?;
        let pts_us = self.units_to_us((self.cursor - self.data_start) as u64 / u64::from(self.unit_bytes));
        self.cursor += i64::from(size);
        Ok(Some(Packet::audio(pts_us, data)))
    }

    fn seek(&mut self, pts_us: i64) -> Result<i64> {
        let total = (self.data_end - self.data_start) as u64 / u64::from(self.unit_bytes);
        let units = self.us_to_units(pts_us).min(total);
        self.cursor = self.data_start + (units * u64::from(self.unit_bytes)) as i64;
        self.reader.seek(self.cursor);
        Ok(self.units_to_us(units))
    }

    fn buffer(&self) -> &Buffer {
        &self.reader
    }
}
