/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::error;
use idf::{Error, Result};

use crate::display::{ImageFormat, Rotation, Size};
use crate::Config;

#[cfg(target_os = "none")]
mod audio;
#[cfg(target_os = "none")]
mod device;
#[cfg(target_os = "none")]
pub(crate) use audio::AudioDriver;
#[cfg(target_os = "none")]
pub(crate) use device::{Display, init};

#[cfg(not(target_os = "none"))]
mod sim;
#[cfg(not(target_os = "none"))]
pub(crate) use sim::{AudioDriver, Display, init};

const TAG: &CStr = c"stopwatch";

const PANEL_SIZE: Size = Size { width: 466, height: 466 };
const PANEL_PPI: u16 = 266;
const PIXEL_FORMAT: ImageFormat = ImageFormat::Rgb565Be;

fn resolve_rotation(config: &Config) -> Result<Rotation> {
    if config.display.fb_num != 0 || config.display.pixel_format.is_some_and(|f| f != PIXEL_FORMAT) {
        error!(TAG, "display: the panel takes {PIXEL_FORMAT:?} without framebuffers");
        return Err(Error::NOT_SUPPORTED);
    }
    let rotation = config.display.rotation.unwrap_or_default();
    if rotation.swaps_axes() {
        error!(TAG, "display: the panel turns only by halves");
        return Err(Error::NOT_SUPPORTED);
    }
    Ok(rotation)
}
