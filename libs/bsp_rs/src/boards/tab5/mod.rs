/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

#[cfg(target_os = "none")]
mod device;
#[cfg(target_os = "none")]
pub(crate) use device::{Display, init};

#[cfg(not(target_os = "none"))]
mod sim;
#[cfg(not(target_os = "none"))]
pub(crate) use sim::{Display, init};

const TAG: &CStr = c"tab5";
