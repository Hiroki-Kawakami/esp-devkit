/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicBool, Ordering};

use idf::{Error, Result};

mod absent;
mod boards;

#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
use boards::tab5 as board;

pub use absent::Absent;

#[non_exhaustive]
pub struct Board {}

#[derive(Default)]
#[non_exhaustive]
pub struct Config {}

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Fails with `INVALID_STATE` after the first call.
pub fn init(config: &Config) -> Result<Board> {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return Err(Error::INVALID_STATE);
    }
    board::init(config)
}
