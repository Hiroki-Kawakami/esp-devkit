/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::error;
use idf::{Error, Result};

use crate::Config;
use crate::display::{ImageFormat, Size};

#[cfg(target_os = "none")]
mod audio;
#[cfg(target_os = "none")]
mod device;
#[cfg(target_os = "none")]
mod power;
#[cfg(target_os = "none")]
pub(crate) use audio::AudioDriver;
#[cfg(target_os = "none")]
pub(crate) use device::{Display, init};

#[cfg(not(target_os = "none"))]
mod sim;
#[cfg(not(target_os = "none"))]
pub(crate) use sim::{AudioDriver, Display, init};

const TAG: &CStr = c"tab5";

const PANEL_SIZE: Size = Size { width: 720, height: 1280 };

fn resolve_fb_num(config: &Config) -> u8 {
    if config.display.fb_num != 0 { config.display.fb_num } else { 2 }
}

fn resolve_pixel_format(config: &Config) -> Result<ImageFormat> {
    match config.display.pixel_format {
        None | Some(ImageFormat::Rgb565) => Ok(ImageFormat::Rgb565),
        Some(ImageFormat::Bgr888) => Ok(ImageFormat::Bgr888),
        Some(other) => {
            error!(TAG, "display: {other:?} is not supported");
            Err(Error::NOT_SUPPORTED)
        }
    }
}
