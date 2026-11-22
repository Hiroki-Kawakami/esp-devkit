/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::error;
use idf::{Error, Result};

use crate::audio::{self, DspMode};
use crate::display::{ImageFormat, Rotation, Size};
use crate::Config;

#[cfg(target_os = "none")]
mod device;
#[cfg(target_os = "none")]
mod power;
#[cfg(target_os = "none")]
mod t5;
#[cfg(target_os = "none")]
pub(crate) use device::{AudioDriver, Display, init};

#[cfg(not(target_os = "none"))]
mod sim;
#[cfg(not(target_os = "none"))]
pub(crate) use sim::{AudioDriver, Display, init};

const TAG: &CStr = c"paper_s3";

const PANEL_SIZE: Size = Size { width: 960, height: 540 };
const PANEL_PPI: u16 = 234;
const PIXEL_FORMAT: ImageFormat = ImageFormat::L4;

/// The buzzer sounds at its one loudness, so there is no volume to process.
fn buzzer_config(config: &Config) -> audio::Config {
    audio::Config { dsp_mode: DspMode::Disable, speaker_mode: config.audio.speaker_mode }
}

/// The pixel format the panel starts with.
fn resolve_display(config: &Config) -> Result<ImageFormat> {
    let format = config.display.pixel_format.unwrap_or(PIXEL_FORMAT);
    if config.display.fb_num != 0 || !matches!(format, ImageFormat::L4 | ImageFormat::L8) {
        error!(TAG, "display: the panel takes L4 or L8 without framebuffers");
        return Err(Error::NOT_SUPPORTED);
    }
    if config.display.rotation.is_some_and(|r| r != Rotation::R0) {
        error!(TAG, "display: the panel does not turn");
        return Err(Error::NOT_SUPPORTED);
    }
    Ok(format)
}
