/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::info;

use super::TAG;
use crate::{Board, Config};

pub(crate) fn init(_config: &Config) -> Result<Board> {
    info!(TAG, "board: Tab5 (simulator)");
    Ok(Board {})
}
