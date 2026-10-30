/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod alac;
pub mod avi;
mod buffer;
mod demux;
mod error;
pub mod es;
pub mod flac;
mod id3;
pub mod mkv;
pub mod mp4;
pub mod ogg;
mod riff;
mod tags;
mod vorbis;
pub mod wav;

#[cfg(test)]
mod tests;

pub use buffer::{ARENA_ALIGNMENT, Aligned, Arena, Buffer, Interrupter, READ_ALIGNMENT, View};
pub use demux::{AudioCodec, AudioTrack, Container, Demux, Demuxer, Info, OpenOptions, Packet, PacketKind, VideoCodec, VideoTrack, open};
pub use error::{Error, Result};
pub use tags::{COVER_MAX_BYTES, Cover, CoverAt, CoverFormat, Field, Tags, Text};
