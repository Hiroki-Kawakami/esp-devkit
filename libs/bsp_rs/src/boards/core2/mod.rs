/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::error;
use idf::{Error, Result};

use crate::Config;
use crate::display::{ImageFormat, Rotation, Size};
use crate::touch::Zone;

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

const TAG: &CStr = c"core2";

const PANEL_SIZE: Size = Size { width: 320, height: 240 };
const PIXEL_FORMAT: ImageFormat = ImageFormat::Rgb565Be;
/// The three marks printed below the picture, which the touch panel reaches.
const BUTTON_ZONES: [Zone; 3] = [
    Zone { x0: 0, y0: 240, x1: 107, y1: 280 },
    Zone { x0: 107, y0: 240, x1: 214, y1: 280 },
    Zone { x0: 214, y0: 240, x1: 320, y1: 280 },
];

fn resolve_rotation(config: &Config) -> Result<Rotation> {
    if config.display.fb_num != 0 || config.display.pixel_format.is_some_and(|f| f != PIXEL_FORMAT) {
        error!(TAG, "display: the panel takes {PIXEL_FORMAT:?} without framebuffers");
        return Err(Error::NOT_SUPPORTED);
    }
    Ok(config.display.rotation.unwrap_or_default())
}
