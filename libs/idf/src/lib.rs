/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_SOC_* flags are unknown to --check-cfg on chips without them.
#![allow(unexpected_cfgs)]

extern crate alloc;

pub mod cache;
mod error;
pub mod executor;
pub mod freertos;
pub mod fs;
#[cfg(target_os = "none")]
pub mod gpio;
pub mod heap;
pub mod i2c;
#[cfg(all(target_os = "none", CONFIG_SOC_I2S_SUPPORTED))]
pub mod i2s;
#[cfg(target_os = "none")]
pub mod lcd;
#[cfg(target_os = "none")]
pub mod ldo;
#[cfg(target_os = "none")]
pub mod ledc;
pub mod log;
pub mod nvs;
pub mod partition;
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
pub mod ppa;
#[cfg(all(target_os = "none", CONFIG_SOC_SDMMC_HOST_SUPPORTED))]
pub mod sdmmc;
pub mod sync;
pub mod system;
pub mod thread;
pub mod time;
#[cfg(target_os = "none")]
pub mod timer;

pub use error::{Error, Result};

pub mod sys {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
