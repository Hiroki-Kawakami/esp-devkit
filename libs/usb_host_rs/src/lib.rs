/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![allow(unexpected_cfgs)]

extern crate alloc;

mod buffer;
pub mod desc;
mod device;
mod error;
mod hcd;
mod host;
pub mod setup;
#[cfg(all(not(target_os = "none"), not(test)))]
mod sim;
#[cfg(test)]
mod tests;
#[cfg(not(target_os = "none"))]
#[allow(dead_code)]
mod usbip;

pub use buffer::Buffer;
pub use device::{Device, Endpoint, Interface};
pub use error::{Error, Result};
pub use hcd::Speed;
pub use host::{Config, Driver, start};
pub use setup::Setup;
