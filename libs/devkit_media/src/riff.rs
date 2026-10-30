/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec;

use crate::AudioCodec;
use crate::buffer::Buffer;
use crate::tags::{Encoding, Field, Tags};

pub(crate) const fn fourcc(id: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*id)
}

pub(crate) const ID_RIFF: u32 = fourcc(b"RIFF");
pub(crate) const ID_LIST: u32 = fourcc(b"LIST");
pub(crate) const ID_FMT: u32 = fourcc(b"fmt ");
pub(crate) const ID_DATA: u32 = fourcc(b"data");
pub(crate) const ID_INFO: u32 = fourcc(b"INFO");
pub(crate) const ID_ID3_LOWER: u32 = fourcc(b"id3 ");
pub(crate) const ID_ID3: u32 = fourcc(b"ID3 ");

pub(crate) const CHUNK_HEADER_BYTES: i64 = 8;

const WAVE_FORMAT_BYTES: usize = 16;
const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_MPEG: u16 = 0x0050;
const WAVE_FORMAT_MP3: u16 = 0x0055;
const WAVE_FORMAT_IMA_ADPCM: u16 = 0x0011;
const WAVE_FORMAT_AAC: u16 = 0x00FF;
const WAVE_FORMAT_AAC_ADTS: u16 = 0x1600;
const WAVE_FORMAT_AAC_FAAD: u16 = 0x706D;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_EXTRA_LIMIT: usize = 256;
const TEXT_BYTES: usize = 128;

#[derive(Clone, Copy)]
pub(crate) struct Chunk {
    pub fourcc: u32,
    pub size: u32,
}

impl Chunk {
    pub fn read(reader: &Buffer) -> Option<Self> {
        let mut bytes = [0; 8];
        reader.read_all(&mut bytes).then(|| Self {
            fourcc: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            size: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }

    pub fn padded(&self) -> i64 {
        i64::from(self.size) + i64::from(self.size & 1)
    }
}

/// The next chunk header below `limit`, where its body starts and where the
/// following chunk does, the pad byte on an odd size included.
pub(crate) fn next_chunk(reader: &Buffer, limit: i64) -> Option<(Chunk, i64, i64)> {
    if reader.tell() + CHUNK_HEADER_BYTES > limit {
        return None;
    }
    let chunk = Chunk::read(reader)?;
    let body = reader.tell();
    Some((chunk, body, body + chunk.padded()))
}

pub(crate) fn read_fourcc(reader: &Buffer) -> Option<u32> {
    let mut bytes = [0; 4];
    reader.read_all(&mut bytes).then(|| u32::from_le_bytes(bytes))
}

fn audio_codec_of_tag(format_tag: u16) -> AudioCodec {
    match format_tag {
        WAVE_FORMAT_PCM => AudioCodec::Pcm,
        WAVE_FORMAT_MP3 => AudioCodec::Mp3,
        WAVE_FORMAT_MPEG => AudioCodec::Mp2,
        WAVE_FORMAT_IMA_ADPCM => AudioCodec::AdpcmIma,
        WAVE_FORMAT_AAC | WAVE_FORMAT_AAC_ADTS | WAVE_FORMAT_AAC_FAAD => AudioCodec::Aac,
        _ => AudioCodec::Unsupported,
    }
}

pub(crate) struct AudioFormat {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub bitrate_bps: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub block_align: u16,
    pub extra: Box<[u8]>,
}

fn read_extra(reader: &Buffer, available: u32) -> Box<[u8]> {
    let mut size = [0; 2];
    if available < 2 || !reader.read_all(&mut size) {
        return Box::default();
    }
    let extra_size = usize::from(u16::from_le_bytes(size)).min(available as usize - 2);
    if extra_size == 0 || extra_size > WAVE_EXTRA_LIMIT {
        return Box::default();
    }
    let mut extra = vec![0; extra_size].into_boxed_slice();
    if reader.read_all(&mut extra) { extra } else { Box::default() }
}

pub(crate) fn read_wave_format(reader: &Buffer, chunk_size: u32) -> Option<AudioFormat> {
    let mut wave = [0; WAVE_FORMAT_BYTES];
    if (chunk_size as usize) < WAVE_FORMAT_BYTES || !reader.read_all(&mut wave) {
        return None;
    }
    let le16 = |at: usize| u16::from_le_bytes([wave[at], wave[at + 1]]);
    let le32 = |at: usize| u32::from_le_bytes([wave[at], wave[at + 1], wave[at + 2], wave[at + 3]]);
    let format_tag = le16(0);
    let bits = le16(14);
    let mut format = AudioFormat {
        codec: audio_codec_of_tag(format_tag),
        sample_rate: le32(4),
        bitrate_bps: le32(8).wrapping_mul(8),
        channels: le16(2) as u8,
        bits_per_sample: if bits != 0 { bits as u8 } else { 16 },
        block_align: le16(12),
        extra: Box::default(),
    };

    let available = chunk_size - WAVE_FORMAT_BYTES as u32;
    if format_tag == WAVE_FORMAT_EXTENSIBLE {
        // cbSize, valid bits, channel mask, then the subformat GUID.
        let mut tail = [0; 24];
        format.codec = if available as usize >= tail.len() && reader.read_all(&mut tail) {
            audio_codec_of_tag(u16::from_le_bytes([tail[8], tail[9]]))
        } else {
            AudioCodec::Unsupported
        };
        return Some(format);
    }
    format.extra = read_extra(reader, available);
    Some(format)
}

fn info_field(id: u32) -> Option<Field> {
    const INAM: u32 = fourcc(b"INAM");
    const IART: u32 = fourcc(b"IART");
    const IPRD: u32 = fourcc(b"IPRD");
    const ICRD: u32 = fourcc(b"ICRD");
    const IPRT: u32 = fourcc(b"IPRT");
    const ITRK: u32 = fourcc(b"ITRK");
    match id {
        INAM => Some(Field::Title),
        IART => Some(Field::Artist),
        IPRD => Some(Field::Album),
        ICRD => Some(Field::Date),
        IPRT | ITRK => Some(Field::Track),
        _ => None,
    }
}

pub(crate) fn read_info(reader: &Buffer, end: i64, tags: &mut Tags) {
    while let Some((chunk, _, next)) = next_chunk(reader, end) {
        if let Some(field) = info_field(chunk.fourcc).filter(|_| chunk.size != 0) {
            let mut text = [0; TEXT_BYTES];
            let want = (chunk.size as usize).min(TEXT_BYTES);
            if reader.read_all(&mut text[..want]) {
                tags.set(field, &text[..want], Encoding::Latin1);
            }
        }
        if next > end {
            break;
        }
        reader.seek(next);
    }
}
