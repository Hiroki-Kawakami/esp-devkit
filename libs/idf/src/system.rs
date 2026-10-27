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

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}
