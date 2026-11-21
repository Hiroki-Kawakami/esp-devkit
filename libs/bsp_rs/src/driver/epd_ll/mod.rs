/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! A grayscale EPD without a controller, driven line by line from an 8-bit
//! i80 bus: the source data on D0..D7, STH on CS and CL on WR, the rest
//! through the board's `Control`. Each pixel replays its own waveform
//! (`waveform`) on a task of its own.

#[cfg(CONFIG_BSP_RS_DRIVER_EPD_LL)]
mod engine;
pub(crate) mod waveform;

#[cfg(CONFIG_BSP_RS_DRIVER_EPD_LL)]
pub(crate) use engine::*;
