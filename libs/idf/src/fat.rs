/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::ffi::CString;
use core::ffi::{CStr, c_char};
use core::mem;
use core::ptr;

use crate::{Error, Result, sys};

#[derive(Clone, Copy, Debug)]
pub struct FatConfig {
    pub format_if_mount_failed: bool,
    pub max_files: u8,
    pub allocation_unit_size: usize,
}

/// A card mounted as FAT at a VFS path, through `sdmmc` or `sdspi`; unmounted
/// on drop.
pub struct FatMount {
    path: CString,
    card: *mut sys::sdmmc_card_t,
}

unsafe impl Send for FatMount {}

impl FatMount {
    /// `mount` calls the host's IDF mount with the path, the FAT settings
    /// and where the card goes.
    pub(crate) unsafe fn mount(
        path: &CStr,
        config: &FatConfig,
        mount: impl FnOnce(*const c_char, &sys::esp_vfs_fat_mount_config_t, &mut *mut sys::sdmmc_card_t) -> sys::esp_err_t,
    ) -> Result<Self> {
        let mut cfg: sys::esp_vfs_fat_mount_config_t = unsafe { mem::zeroed() };
        cfg.format_if_mount_failed = config.format_if_mount_failed;
        cfg.max_files = i32::from(config.max_files);
        cfg.allocation_unit_size = config.allocation_unit_size;
        let mut card = ptr::null_mut();
        Error::check(mount(path.as_ptr(), &cfg, &mut card))?;
        Ok(Self { path: path.into(), card })
    }

    /// Total and free bytes.
    pub fn info(&self) -> Result<(u64, u64)> {
        let (mut total, mut free) = (0, 0);
        Error::check(unsafe { sys::esp_vfs_fat_info(self.path.as_ptr(), &mut total, &mut free) })?;
        Ok((total, free))
    }

    pub fn unmount(self) -> Result<()> {
        let mount = mem::ManuallyDrop::new(self);
        let path = unsafe { ptr::read(&mount.path) };
        Error::check(unsafe { sys::esp_vfs_fat_sdcard_unmount(path.as_ptr(), mount.card) })
    }
}

impl Drop for FatMount {
    fn drop(&mut self) {
        unsafe { sys::esp_vfs_fat_sdcard_unmount(self.path.as_ptr(), self.card) };
    }
}
