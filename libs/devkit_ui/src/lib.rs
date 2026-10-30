/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

mod presenter;

pub use bsp_rs::display::Rotation;
pub use presenter::{Config, PresentMode, Presenter, Stats};
