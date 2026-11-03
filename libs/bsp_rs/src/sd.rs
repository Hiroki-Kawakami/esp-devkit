/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;

use idf::sync::{Mutex, OnceLock};
use idf::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub struct MountConfig {
    pub format_if_mount_failed: bool,
    /// Files open at once; 0 selects 5.
    pub max_files: u8,
    /// `None` selects the board's speed.
    pub max_freq_khz: Option<u32>,
    /// Lets reads and writes use buffers in PSRAM, through a bounce buffer
    /// there; `NOT_SUPPORTED` where the controller cannot reach PSRAM.
    pub psram_bounce_buffer: bool,
}

pub(crate) trait Driver: Send + 'static {
    fn mount(&mut self, path: &CStr, config: &MountConfig) -> Result<()>;
    fn unmount(&mut self) -> Result<()>;
    fn is_mounted(&self) -> bool;
}

/// The card slot, shared by every task.
pub struct Sd(Mutex<Box<dyn Driver>>);

impl Sd {
    /// Mounts the card's FAT file system at `path`, for `idf::fs`.
    /// `INVALID_STATE` while it is mounted.
    pub fn mount(&self, path: &CStr, config: &MountConfig) -> Result<()> {
        let mut driver = self.0.lock();
        if driver.is_mounted() {
            return Err(Error::INVALID_STATE);
        }
        driver.mount(path, config)
    }

    /// Files still open under the mount point stop working.
    pub fn unmount(&self) -> Result<()> {
        let mut driver = self.0.lock();
        if !driver.is_mounted() {
            return Err(Error::INVALID_STATE);
        }
        driver.unmount()
    }

    pub fn is_mounted(&self) -> bool {
        self.0.lock().is_mounted()
    }
}

static SD: OnceLock<Sd> = OnceLock::new();

pub(crate) fn start(driver: impl Driver) -> Result<&'static Sd> {
    Ok(SD.get_or_init(|| Sd(Mutex::new(Box::new(driver) as Box<dyn Driver>))))
}
