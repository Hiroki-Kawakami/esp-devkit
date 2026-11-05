/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::{CStr, c_char};

use crate::{Error, Result, sys};

pub const CORE_COUNT: u32 = sdkconfig::CONFIG_FREERTOS_NUMBER_OF_CORES as u32;

// A task not pinned to a core may run on another one as soon as this returns.
pub fn core_id() -> u32 {
    unsafe { sys::xPortGetCoreID() as u32 }
}

/// From the hardware RNG; truly random only while the RF or the bootloader's
/// entropy source runs.
pub fn random() -> u32 {
    unsafe { sys::esp_random() }
}

/// The MAC address burnt into this chip's eFuse at the factory.
pub fn factory_mac() -> Result<[u8; 6]> {
    let mut mac = [0; 6];
    Error::check(unsafe { sys::esp_read_mac(mac.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_EFUSE_FACTORY) })?;
    Ok(mac)
}

pub fn in_isr() -> bool {
    #[cfg(target_os = "none")]
    return unsafe { sys::xPortInIsrContext() != 0 };
    #[cfg(not(target_os = "none"))]
    false
}

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}

#[derive(Clone, Copy)]
pub struct AppDescription(&'static sys::esp_app_desc_t);

pub fn app_description() -> AppDescription {
    AppDescription(unsafe { &*sys::esp_app_get_description() })
}

impl AppDescription {
    pub fn version(&self) -> &'static str {
        field(&self.0.version)
    }

    pub fn project_name(&self) -> &'static str {
        field(&self.0.project_name)
    }

    pub fn time(&self) -> &'static str {
        field(&self.0.time)
    }

    pub fn date(&self) -> &'static str {
        field(&self.0.date)
    }

    pub fn idf_version(&self) -> &'static str {
        field(&self.0.idf_ver)
    }
}

fn field(chars: &'static [c_char]) -> &'static str {
    let bytes = unsafe { core::slice::from_raw_parts(chars.as_ptr().cast::<u8>(), chars.len()) };
    let bytes = CStr::from_bytes_until_nul(bytes).map_or(bytes, CStr::to_bytes);
    core::str::from_utf8(bytes).unwrap_or("")
}
