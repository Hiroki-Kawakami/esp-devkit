/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::{Format, Pcm, PcmMut, Result};

/// Codecs work on one encoded frame at a time; containers are parsed by the
/// caller. A concrete decoder also has a `decode` typed for its samples; this
/// trait is for a caller that picks the codec at run time.
pub trait Decoder {
    /// `pcm` holds [`max_frames`](Self::max_frames) frames in the decoder's
    /// format. Returns the frames written.
    fn decode(&mut self, frame: &[u8], pcm: PcmMut<'_>) -> Result<usize>;
    /// Drops what the decoder carries from one frame to the next, as for a seek.
    fn reset(&mut self);
    fn max_frames(&self) -> usize;
    fn format(&self) -> Format;
    fn channels(&self) -> u8;
}

pub trait Encoder {
    /// `pcm` is [`frame_frames`](Self::frame_frames) frames except for the
    /// last call; `out` holds [`max_bytes`](Self::max_bytes). Returns the bytes
    /// written.
    fn encode(&mut self, pcm: Pcm<'_>, out: &mut [u8]) -> Result<usize>;
    fn frame_frames(&self) -> usize;
    fn max_bytes(&self) -> usize;
    fn format(&self) -> Format;
    fn channels(&self) -> u8;
}
