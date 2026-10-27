/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::sys;

pub fn restart() -> ! {
    unsafe { sys::esp_restart() }
}
