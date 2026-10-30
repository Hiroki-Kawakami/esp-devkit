/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![feature(optimize_attribute)]

extern crate alloc;

pub mod aac;
pub mod adpcm;
pub mod alac;
mod bits;
mod codec;
pub mod convert;
pub mod drift;
pub mod eq;
mod error;
pub mod fifo;
pub mod flac;
pub mod gain;
pub mod graph;
pub mod mixer;
pub mod mp3;
pub mod opus;
pub mod resampler;
mod sample;
mod sync;
#[cfg(test)]
mod tests;

include!(concat!(env!("OUT_DIR"), "/pie.rs"));

pub use codec::{Decoder, Encoder};
pub use error::{Error, Result};
pub use sample::{Format, MAX_CHANNELS, Pcm, PcmMut, Sample};
