/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::ffi::CString;
use core::ffi::{CStr, c_char};
use core::mem::MaybeUninit;

use idf::{Error, Result};

use super::ffi;
use crate::sd::{Driver, MountConfig, Space};

const ENV_ROOT: &CStr = c"SIMULATOR_SDCARD_PATH";
const DEFAULT_ROOT: &CStr = c"simulator/sdcard";

struct Mounted {
    path: CString,
    root: CString,
}

/// Mounting maps the mount point onto a host directory.
pub(crate) struct SimSd {
    mounted: Option<Mounted>,
}

impl SimSd {
    pub(crate) fn new() -> Self {
        Self { mounted: None }
    }
}

impl Driver for SimSd {
    fn mount(&mut self, path: &CStr, _config: &MountConfig) -> Result<()> {
        let env = unsafe { ffi::getenv(ENV_ROOT.as_ptr()) };
        let root: *const c_char = if env.is_null() { DEFAULT_ROOT.as_ptr() } else { env };
        Error::check(unsafe { ffi::sim_path_redirect_add(path.as_ptr(), root) })?;
        self.mounted = Some(Mounted { path: path.into(), root: unsafe { CStr::from_ptr(root) }.into() });
        Ok(())
    }

    fn unmount(&mut self) -> Result<()> {
        let mounted = self.mounted.take().ok_or(Error::INVALID_STATE)?;
        unsafe { ffi::sim_path_redirect_remove(mounted.path.as_ptr()) };
        Ok(())
    }

    fn is_mounted(&self) -> bool {
        self.mounted.is_some()
    }

    fn space(&self) -> Result<Space> {
        let mounted = self.mounted.as_ref().ok_or(Error::INVALID_STATE)?;
        let mut info = MaybeUninit::<ffi::statvfs>::uninit();
        if unsafe { ffi::statvfs(mounted.root.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(Error::FAIL);
        }
        let info = unsafe { info.assume_init() };
        let unit = info.f_frsize as u64;
        Ok(Space { total: info.f_blocks as u64 * unit, free: info.f_bavail as u64 * unit })
    }
}
