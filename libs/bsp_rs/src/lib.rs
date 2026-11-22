/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_SOC_* flags are unknown to --check-cfg on chips without them.
#![allow(unexpected_cfgs)]

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use idf::{Error, Result};

mod absent;
// Each board starts only the subsystems it has, so on the others these
// modules are legitimately unused.
#[allow(dead_code)]
pub mod audio;
mod boards;
#[allow(dead_code)]
pub mod button;
mod devices;
pub mod dispatch;
#[allow(dead_code)]
pub mod display;
mod driver;
#[allow(dead_code)]
pub mod imu;
pub mod power;
#[allow(dead_code)]
pub mod rtc;
#[allow(dead_code)]
pub mod sd;
#[cfg(not(target_os = "none"))]
mod sim;
#[allow(dead_code)]
pub mod touch;

#[cfg(CONFIG_BSP_RS_BOARD_ATOM_S3_LITE)]
use boards::atom_s3_lite as board;
#[cfg(CONFIG_BSP_RS_BOARD_CORE2)]
use boards::core2 as board;
#[cfg(CONFIG_BSP_RS_BOARD_CORE_S3)]
use boards::core_s3 as board;
#[cfg(CONFIG_BSP_RS_BOARD_PAPER)]
use boards::paper as board;
#[cfg(CONFIG_BSP_RS_BOARD_PAPER_S3)]
use boards::paper_s3 as board;
#[cfg(CONFIG_BSP_RS_BOARD_STOPWATCH)]
use boards::stopwatch as board;
#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
use boards::tab5 as board;

pub use absent::Absent;

#[non_exhaustive]
pub struct Board {
    pub audio: Option<&'static audio::Audio>,
    pub buttons: Option<&'static button::Buttons>,
    pub display: Option<&'static display::Display>,
    pub imu: Option<&'static imu::Imu>,
    pub power: &'static power::Power,
    pub rtc: Option<&'static rtc::Rtc>,
    pub sd: Option<&'static sd::Sd>,
    pub touch: Option<&'static touch::Touch>,
}

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
    pub audio: audio::Config,
    pub display: display::Config,
    pub dispatch: dispatch::Config,
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Fails with `INVALID_STATE` after the first call.
pub fn init(config: &Config) -> Result<Board> {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return Err(Error::INVALID_STATE);
    }
    dispatch::configure(&config.dispatch);
    board::init(config)
}
