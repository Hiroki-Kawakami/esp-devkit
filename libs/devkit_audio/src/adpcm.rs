/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::sample::check_channels;
use crate::{Decoder, Encoder, Error, Format, MAX_CHANNELS, Pcm, PcmMut, Result};

const INDEX_TABLE: [i8; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

const STEP_TABLE: [i16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// IMA ADPCM in the WAV block layout (format tag 0x11), S16 PCM.
#[derive(Clone, Copy, Debug)]
pub struct AdpcmConfig {
    pub channels: u8,
    /// Bytes per block, a multiple of `4 * channels`.
    pub block_align: u16,
}

#[derive(Clone, Copy, Default)]
struct ChannelState {
    predictor: i32,
    index: i32,
}

impl ChannelState {
    fn expand(&mut self, nibble: u8) -> i16 {
        let step = i32::from(STEP_TABLE[self.index as usize]);
        let mut diff = step >> 3;
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 4 != 0 {
            diff += step;
        }
        self.predictor += if nibble & 8 != 0 { -diff } else { diff };
        self.predictor = self.predictor.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + i32::from(INDEX_TABLE[usize::from(nibble)])).clamp(0, 88);
        self.predictor as i16
    }

    fn compress(&mut self, sample: i16) -> u8 {
        let mut step = i32::from(STEP_TABLE[self.index as usize]);
        let mut diff = i32::from(sample) - self.predictor;
        let mut nibble = 0;
        if diff < 0 {
            nibble = 8;
            diff = -diff;
        }
        let mut mask = 4;
        while mask != 0 {
            if diff >= step {
                nibble |= mask;
                diff -= step;
            }
            step >>= 1;
            mask >>= 1;
        }
        self.expand(nibble);
        nibble
    }
}

impl AdpcmConfig {
    fn check(&self) -> Result<()> {
        check_channels(self.channels)?;
        let group = 4 * u16::from(self.channels);
        if self.block_align > group && self.block_align % group == 0 { Ok(()) } else { Err(Error::InvalidArg) }
    }

    /// PCM frames of a whole block; 0 for an invalid config.
    pub fn block_frames(&self) -> usize {
        if self.check().is_err() {
            return 0;
        }
        (usize::from(self.block_align) - 4 * usize::from(self.channels)) * 2 / usize::from(self.channels) + 1
    }
}

pub struct AdpcmDecoder {
    channels: u8,
    max_frames: usize,
}

impl AdpcmDecoder {
    /// A short last block decodes to the frames it holds.
    pub fn new(config: &AdpcmConfig) -> Result<Self> {
        config.check()?;
        Ok(Self { channels: config.channels, max_frames: config.block_frames() })
    }

    pub fn decode(&mut self, frame: &[u8], pcm: &mut [i16]) -> Result<usize> {
        let channels = usize::from(self.channels);
        let group = 4 * channels;
        if frame.len() < group || frame.len() % group != 0 || (frame.len() - group) / group * 8 + 1 > self.max_frames {
            return Err(Error::InvalidSize);
        }
        let groups = (frame.len() - group) / group;
        let frames = 1 + groups * 8;
        if pcm.len() < frames * channels {
            return Err(Error::InvalidSize);
        }
        let mut state = [ChannelState::default(); MAX_CHANNELS as usize];
        for (ch, s) in state[..channels].iter_mut().enumerate() {
            let h = &frame[ch * 4..];
            s.predictor = i32::from(i16::from_le_bytes([h[0], h[1]]));
            s.index = i32::from(h[2].min(88));
            pcm[ch] = s.predictor as i16;
        }
        let data = &frame[group..];
        for g in 0..groups {
            for ch in 0..channels {
                let bytes = &data[(g * channels + ch) * 4..];
                for b in 0..4 {
                    let f = 1 + g * 8 + b * 2;
                    pcm[f * channels + ch] = state[ch].expand(bytes[b] & 0x0f);
                    pcm[(f + 1) * channels + ch] = state[ch].expand(bytes[b] >> 4);
                }
            }
        }
        Ok(frames)
    }
}

impl Decoder for AdpcmDecoder {
    fn decode(&mut self, frame: &[u8], pcm: PcmMut<'_>) -> Result<usize> {
        match pcm {
            PcmMut::S16(pcm) => AdpcmDecoder::decode(self, frame, pcm),
            PcmMut::S32(_) => Err(Error::InvalidArg),
        }
    }

    fn reset(&mut self) {}

    fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn format(&self) -> Format {
        Format::S16
    }

    fn channels(&self) -> u8 {
        self.channels
    }
}

pub struct AdpcmEncoder {
    channels: u8,
    frame_frames: usize,
    max_bytes: usize,
    state: [ChannelState; MAX_CHANNELS as usize],
}

impl AdpcmEncoder {
    /// A short last input is padded with its final frame up to a whole group of
    /// eight, so it decodes a few frames longer.
    pub fn new(config: &AdpcmConfig) -> Result<Self> {
        config.check()?;
        Ok(Self {
            channels: config.channels,
            frame_frames: config.block_frames(),
            max_bytes: usize::from(config.block_align),
            state: [ChannelState::default(); MAX_CHANNELS as usize],
        })
    }

    pub fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> Result<usize> {
        let channels = usize::from(self.channels);
        let frames = pcm.len() / channels;
        if frames == 0 || frames > self.frame_frames {
            return Err(Error::InvalidSize);
        }
        let groups = (frames - 1).div_ceil(8);
        let len = 4 * channels * (1 + groups);
        if out.len() < len {
            return Err(Error::InvalidSize);
        }
        for ch in 0..channels {
            let first = pcm[ch];
            self.state[ch].predictor = i32::from(first);
            let h = &mut out[ch * 4..ch * 4 + 4];
            h[..2].copy_from_slice(&first.to_le_bytes());
            h[2] = self.state[ch].index as u8;
            h[3] = 0;
        }
        let (_, data) = out.split_at_mut(4 * channels);
        for g in 0..groups {
            for ch in 0..channels {
                let bytes = &mut data[(g * channels + ch) * 4..];
                for (b, byte) in bytes[..4].iter_mut().enumerate() {
                    let f = 1 + g * 8 + b * 2;
                    let lo = self.state[ch].compress(pcm[f.min(frames - 1) * channels + ch]);
                    let hi = self.state[ch].compress(pcm[(f + 1).min(frames - 1) * channels + ch]);
                    *byte = lo | hi << 4;
                }
            }
        }
        Ok(len)
    }
}

impl Encoder for AdpcmEncoder {
    fn encode(&mut self, pcm: Pcm<'_>, out: &mut [u8]) -> Result<usize> {
        match pcm {
            Pcm::S16(pcm) => AdpcmEncoder::encode(self, pcm, out),
            Pcm::S32(_) => Err(Error::InvalidArg),
        }
    }

    fn frame_frames(&self) -> usize {
        self.frame_frames
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn format(&self) -> Format {
        Format::S16
    }

    fn channels(&self) -> u8 {
        self.channels
    }
}
