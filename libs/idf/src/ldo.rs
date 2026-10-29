/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::mem;
use core::ptr;

use crate::{Error, Result, sys};

pub struct Channel(sys::esp_ldo_channel_handle_t);

unsafe impl Send for Channel {}
unsafe impl Sync for Channel {}

impl Channel {
    pub fn acquire(id: i32, voltage_mv: i32) -> Result<Self> {
        let mut cfg: sys::esp_ldo_channel_config_t = unsafe { mem::zeroed() };
        cfg.chan_id = id;
        cfg.voltage_mv = voltage_mv;
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::esp_ldo_acquire_channel(&cfg, &mut handle) })?;
        Ok(Self(handle))
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        unsafe { sys::esp_ldo_release_channel(self.0) };
    }
}
