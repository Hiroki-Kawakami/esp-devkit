/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use crate::Config;
use crate::display::{PixelFormat, Size};

#[cfg(target_os = "none")]
mod device;
#[cfg(target_os = "none")]
pub(crate) use device::{Display, init};

#[cfg(not(target_os = "none"))]
mod sim;
#[cfg(not(target_os = "none"))]
pub(crate) use sim::{Display, init};

const TAG: &CStr = c"tab5";

const PANEL_SIZE: Size = Size { width: 720, height: 1280 };

fn resolve_fb_num(config: &Config) -> u8 {
    if config.display.fb_num != 0 { config.display.fb_num } else { 2 }
}

fn resolve_pixel_format(config: &Config) -> PixelFormat {
    match config.display.pixel_format {
        Some(PixelFormat::Rgb888) => PixelFormat::Rgb888,
        _ => PixelFormat::Rgb565,
    }
}
