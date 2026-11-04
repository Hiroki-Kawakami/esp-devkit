/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::sync::Mutex;
use crate::{Error, Result, sys};

static READY: Mutex<bool> = Mutex::new(false);

/// Initializes the default NVS partition; once it succeeded, later calls
/// return at once. A partition without free pages or written by a newer
/// IDF is erased, losing what it held, and initialized again.
pub fn init() -> Result<()> {
    let mut ready = READY.lock();
    if *ready {
        return Ok(());
    }
    let mut code = unsafe { sys::nvs_flash_init() };
    if code == sys::ESP_ERR_NVS_NO_FREE_PAGES as sys::esp_err_t
        || code == sys::ESP_ERR_NVS_NEW_VERSION_FOUND as sys::esp_err_t
    {
        Error::check(unsafe { sys::nvs_flash_erase() })?;
        code = unsafe { sys::nvs_flash_init() };
    }
    Error::check(code)?;
    *ready = true;
    Ok(())
}
