/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::error;
use idf::{Error, Result};

use crate::display::{ImageFormat, Rotation, Size};
use crate::{Config, power};

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

const TAG: &CStr = c"core_s3";

const PANEL_SIZE: Size = Size { width: 320, height: 240 };
const PIXEL_FORMAT: ImageFormat = ImageFormat::Rgb565Be;

struct UnknownPower;

impl power::Driver for UnknownPower {}

fn resolve_rotation(config: &Config) -> Result<Rotation> {
    if config.display.fb_num != 0 || config.display.pixel_format.is_some_and(|f| f != PIXEL_FORMAT) {
        error!(TAG, "display: the panel takes {PIXEL_FORMAT:?} without framebuffers");
        return Err(Error::NOT_SUPPORTED);
    }
    Ok(config.display.rotation.unwrap_or_default())
}
