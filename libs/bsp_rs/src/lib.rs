/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

use idf::{Error, Result};

mod absent;
mod boards;
mod devices;
pub mod dispatch;
pub mod display;
mod driver;
#[cfg(not(target_os = "none"))]
mod sim;
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
pub mod touch;

#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
use boards::tab5 as board;

pub use absent::Absent;

#[non_exhaustive]
pub struct Board {
    pub display: Option<&'static display::Display>,
    pub touch: Option<&'static touch::Touch>,
}

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
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
