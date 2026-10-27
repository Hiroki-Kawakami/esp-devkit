/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::fmt;
use core::num::NonZeroI32;

use crate::sys;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Error(NonZeroI32);

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub const FAIL: Self = Self::constant(sys::ESP_FAIL);
    pub const NO_MEM: Self = Self::constant(sys::ESP_ERR_NO_MEM as i32);
    pub const INVALID_ARG: Self = Self::constant(sys::ESP_ERR_INVALID_ARG as i32);
    pub const INVALID_STATE: Self = Self::constant(sys::ESP_ERR_INVALID_STATE as i32);
    pub const INVALID_SIZE: Self = Self::constant(sys::ESP_ERR_INVALID_SIZE as i32);
    pub const NOT_FOUND: Self = Self::constant(sys::ESP_ERR_NOT_FOUND as i32);
    pub const NOT_SUPPORTED: Self = Self::constant(sys::ESP_ERR_NOT_SUPPORTED as i32);
    pub const TIMEOUT: Self = Self::constant(sys::ESP_ERR_TIMEOUT as i32);
    pub const INVALID_RESPONSE: Self = Self::constant(sys::ESP_ERR_INVALID_RESPONSE as i32);
    pub const INVALID_CRC: Self = Self::constant(sys::ESP_ERR_INVALID_CRC as i32);
    pub const INVALID_VERSION: Self = Self::constant(sys::ESP_ERR_INVALID_VERSION as i32);
    pub const INVALID_MAC: Self = Self::constant(sys::ESP_ERR_INVALID_MAC as i32);
    pub const NOT_FINISHED: Self = Self::constant(sys::ESP_ERR_NOT_FINISHED as i32);
    pub const NOT_ALLOWED: Self = Self::constant(sys::ESP_ERR_NOT_ALLOWED as i32);

    const fn constant(code: sys::esp_err_t) -> Self {
        match Self::from_code(code) {
            Some(e) => e,
            None => panic!("ESP_OK is not an error"),
        }
    }

    pub const fn from_code(code: sys::esp_err_t) -> Option<Self> {
        match NonZeroI32::new(code) {
            Some(code) => Some(Self(code)),
            None => None,
        }
    }

    pub const fn check(code: sys::esp_err_t) -> Result<()> {
        match Self::from_code(code) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub const fn code(self) -> sys::esp_err_t {
        self.0.get()
    }

    pub fn name(self) -> &'static str {
        let name = unsafe { CStr::from_ptr(sys::esp_err_to_name(self.code())) };
        name.to_str().unwrap_or("UNKNOWN")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({:#x})", self.name(), self.code())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error({}, {:#x})", self.name(), self.code())
    }
}

impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn from_code_and_check() {
        assert_eq!(Error::from_code(0), None);
        assert_eq!(Error::from_code(0x101), Some(Error::NO_MEM));
        assert_eq!(Error::check(0), Ok(()));
        assert_eq!(Error::check(-1), Err(Error::FAIL));
        assert_eq!(Error::TIMEOUT.code(), 0x107);
    }

    #[test]
    fn formats_name_and_code() {
        assert_eq!(Error::NO_MEM.name(), "ESP_ERR_NO_MEM");
        assert_eq!(format!("{}", Error::NO_MEM), "ESP_ERR_NO_MEM (0x101)");
        assert_eq!(format!("{:?}", Error::INVALID_ARG), "Error(ESP_ERR_INVALID_ARG, 0x102)");
        assert_eq!(format!("{}", Error::FAIL), "ESP_FAIL (0xffffffff)");
    }

    #[test]
    fn propagates_with_question_mark() {
        fn open(rc: i32) -> crate::Result<u8> {
            Error::check(rc)?;
            Ok(1)
        }
        assert_eq!(open(0), Ok(1));
        assert_eq!(open(0x105), Err(Error::NOT_FOUND));
    }
}
