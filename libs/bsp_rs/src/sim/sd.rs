/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::ffi::CString;
use core::ffi::CStr;

use idf::{Error, Result};

use super::ffi;
use crate::sd::{Driver, MountConfig};

const ENV_ROOT: &CStr = c"SIMULATOR_SDCARD_PATH";
const DEFAULT_ROOT: &CStr = c"simulator/sdcard";

/// Mounting maps the mount point onto a host directory.
pub(crate) struct SimSd {
    mounted: Option<CString>,
}

impl SimSd {
    pub(crate) fn new() -> Self {
        Self { mounted: None }
    }
}

impl Driver for SimSd {
    fn mount(&mut self, path: &CStr, _config: &MountConfig) -> Result<()> {
        let env = unsafe { ffi::getenv(ENV_ROOT.as_ptr()) };
        let root = if env.is_null() { DEFAULT_ROOT.as_ptr() } else { env };
        Error::check(unsafe { ffi::sim_path_redirect_add(path.as_ptr(), root) })?;
        self.mounted = Some(path.into());
        Ok(())
    }

    fn unmount(&mut self) -> Result<()> {
        let path = self.mounted.take().ok_or(Error::INVALID_STATE)?;
        unsafe { ffi::sim_path_redirect_remove(path.as_ptr()) };
        Ok(())
    }

    fn is_mounted(&self) -> bool {
        self.mounted.is_some()
    }
}
