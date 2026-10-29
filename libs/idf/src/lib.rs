/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod error;
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
pub mod sync;
pub mod system;
pub mod thread;
pub mod time;

pub use error::{Error, Result};

pub mod sys {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
