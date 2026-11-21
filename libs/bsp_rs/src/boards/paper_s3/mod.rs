/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::log::error;
use idf::{Error, Result};

use crate::display::{ImageFormat, Rotation, Size};
use crate::{Absent, Config};

#[cfg(target_os = "none")]
mod device;
#[cfg(target_os = "none")]
mod power;
#[cfg(target_os = "none")]
mod t5;
#[cfg(target_os = "none")]
pub(crate) use device::{Display, init};

#[cfg(not(target_os = "none"))]
mod sim;
#[cfg(not(target_os = "none"))]
pub(crate) use sim::{Display, init};

pub(crate) type AudioDriver = Absent;

const TAG: &CStr = c"paper_s3";

const PANEL_SIZE: Size = Size { width: 960, height: 540 };
const PANEL_PPI: u16 = 234;
const PIXEL_FORMAT: ImageFormat = ImageFormat::L8;

fn check_display(config: &Config) -> Result<()> {
    if config.display.fb_num != 0 || config.display.pixel_format.is_some_and(|f| f != PIXEL_FORMAT) {
        error!(TAG, "display: the panel takes {PIXEL_FORMAT:?} without framebuffers");
        return Err(Error::NOT_SUPPORTED);
    }
    if config.display.rotation.is_some_and(|r| r != Rotation::R0) {
        error!(TAG, "display: the panel does not turn");
        return Err(Error::NOT_SUPPORTED);
    }
    Ok(())
}
