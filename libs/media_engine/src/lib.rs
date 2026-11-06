/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[cfg(any(not(target_os = "none"), CONFIG_IDF_TARGET_ESP32P4))]
mod ppa;

#[cfg(any(not(target_os = "none"), CONFIG_IDF_TARGET_ESP32P4))]
pub use ppa::{Alpha, Background, Blend, Fill, Job, Ppa, Slot, Srm, Yuv, YuvRange, YuvStandard, ppa};

#[cfg(CONFIG_IDF_TARGET_ESP32P4)]
#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unsafe_op_in_unsafe_fn)]
#[allow(clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
