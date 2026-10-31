/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use crate::sync::Mutex;
use crate::{Error, Result, sys};

const MAX_NAMESPACE: usize = 15;

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

/// A namespace of the default partition, open for reading and writing.
pub struct Nvs {
    handle: sys::nvs_handle_t,
}

unsafe impl Send for Nvs {}

impl Nvs {
    /// Created when missing.
    pub fn open(namespace: &CStr) -> Result<Self> {
        if namespace.to_bytes().len() > MAX_NAMESPACE {
            return Err(Error::INVALID_ARG);
        }
        let mut handle = 0;
        Error::check(unsafe { sys::nvs_open(namespace.as_ptr(), sys::nvs_open_mode_t_NVS_READWRITE, &mut handle) })?;
        Ok(Self { handle })
    }

    pub fn get_u8(&self, key: &CStr) -> Result<Option<u8>> {
        let mut value = 0;
        let code = unsafe { sys::nvs_get_u8(self.handle, key.as_ptr(), &mut value) };
        if code == sys::ESP_ERR_NVS_NOT_FOUND as i32 {
            return Ok(None);
        }
        Error::check(code).map(|()| Some(value))
    }

    /// Lasts once `commit` has returned.
    pub fn set_u8(&mut self, key: &CStr, value: u8) -> Result<()> {
        Error::check(unsafe { sys::nvs_set_u8(self.handle, key.as_ptr(), value) })
    }

    pub fn commit(&mut self) -> Result<()> {
        Error::check(unsafe { sys::nvs_commit(self.handle) })
    }
}

impl Drop for Nvs {
    fn drop(&mut self) {
        unsafe { sys::nvs_close(self.handle) };
    }
}
