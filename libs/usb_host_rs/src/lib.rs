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
#[cfg(all(target_os = "none", CONFIG_SOC_USB_OTG_SUPPORTED))]
mod dwc2;
mod error;
mod hcd;
mod host;
pub mod setup;
mod watch;
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
pub use device::DeviceId;
pub use host::{Config, start};
pub use watch::{Event, Watch, devices, watch};
pub use setup::Setup;
