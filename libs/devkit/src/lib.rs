/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

mod bitflags;
mod defer;
mod fmt_buf;

pub use defer::{Guard, guard};
pub use fmt_buf::FmtBuf;
