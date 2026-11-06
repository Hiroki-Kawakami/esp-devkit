/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_SOC_* flags are unknown to --check-cfg on chips without them.
#![allow(unexpected_cfgs)]

extern crate alloc;

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
mod ppa;

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
pub use ppa::{Alpha, Background, Blend, Fill, Job, Ppa, Slot, Srm, Yuv, YuvRange, YuvStandard, ppa};

#[cfg(CONFIG_SOC_PPA_SUPPORTED)]
#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unsafe_op_in_unsafe_fn)]
#[allow(clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
