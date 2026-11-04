/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::sys;

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

pub fn in_isr() -> bool {
    #[cfg(target_os = "none")]
    return unsafe { sys::xPortInIsrContext() != 0 };
    #[cfg(not(target_os = "none"))]
    false
}

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}
