/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_SOC_* flags are unknown to --check-cfg on chips without them.
#![allow(unexpected_cfgs)]

extern crate alloc;

mod error;
pub mod fs;
#[cfg(target_os = "none")]
pub mod gpio;
pub mod heap;
pub mod i2c;
#[cfg(target_os = "none")]
pub mod lcd;
#[cfg(target_os = "none")]
pub mod ldo;
#[cfg(target_os = "none")]
pub mod ledc;
pub mod log;
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
pub mod ppa;
pub mod sync;
pub mod system;
pub mod thread;
pub mod time;

pub use error::{Error, Result};

pub mod sys {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
