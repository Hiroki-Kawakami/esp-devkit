/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::ffi::CString;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::marker::PhantomData;

use crate::sync::Mutex;
use crate::{Error, Result, sys};

const MAX_NAME: usize = 15;
const NOT_FOUND: sys::esp_err_t = sys::ESP_ERR_NVS_NOT_FOUND as sys::esp_err_t;

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

fn found(code: sys::esp_err_t) -> Result<bool> {
    if code == NOT_FOUND {
        return Ok(false);
    }
    Error::check(code).map(|()| true)
}

pub struct Key<T> {
    name: &'static CStr,
    value: PhantomData<fn() -> T>,
}

impl<T> Key<T> {
    /// Panics past 15 bytes, which in a `const` fails the build.
    pub const fn new(name: &'static CStr) -> Self {
        assert!(name.to_bytes().len() <= MAX_NAME, "NVS keys are 15 bytes at most");
        Self { name, value: PhantomData }
    }

    pub const fn name(self) -> &'static CStr {
        self.name
    }
}

impl<T> Clone for Key<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Key<T> {}

impl<T> PartialEq for Key<T> {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl<T> Eq for Key<T> {}

mod sealed {
    pub trait Sealed {}
}

/// A type NVS stores natively.
pub trait Raw: sealed::Sealed + Sized {
    type Arg<'a>;

    #[doc(hidden)]
    fn get(handle: sys::nvs_handle_t, key: &CStr) -> Result<Option<Self>>;
    #[doc(hidden)]
    fn set(handle: sys::nvs_handle_t, key: &CStr, value: Self::Arg<'_>) -> Result<()>;
}

/// What a `Key` holds, kept as its `Raw` form.
pub trait Value: Sized {
    type Raw: Raw;
    type Arg<'a>;

    fn from_raw(raw: Self::Raw) -> Option<Self>;
    fn to_raw<'a>(value: Self::Arg<'a>) -> <Self::Raw as Raw>::Arg<'a>;
}

macro_rules! int {
    ($($ty:ty: $get:ident $set:ident),* $(,)?) => {$(
        impl sealed::Sealed for $ty {}

        impl Raw for $ty {
            type Arg<'a> = $ty;

            fn get(handle: sys::nvs_handle_t, key: &CStr) -> Result<Option<Self>> {
                let mut value = 0;
                let code = unsafe { sys::$get(handle, key.as_ptr(), &mut value) };
                found(code).map(|found| found.then_some(value))
            }

            fn set(handle: sys::nvs_handle_t, key: &CStr, value: $ty) -> Result<()> {
                Error::check(unsafe { sys::$set(handle, key.as_ptr(), value) })
            }
        }

        impl Value for $ty {
            type Raw = $ty;
            type Arg<'a> = $ty;

            fn from_raw(raw: $ty) -> Option<Self> {
                Some(raw)
            }

            fn to_raw<'a>(value: Self::Arg<'a>) -> <Self::Raw as Raw>::Arg<'a> {
                value
            }
        }
    )*};
}

int! {
    u8: nvs_get_u8 nvs_set_u8,
    i8: nvs_get_i8 nvs_set_i8,
    u16: nvs_get_u16 nvs_set_u16,
    i16: nvs_get_i16 nvs_set_i16,
    u32: nvs_get_u32 nvs_set_u32,
    i32: nvs_get_i32 nvs_set_i32,
    u64: nvs_get_u64 nvs_set_u64,
    i64: nvs_get_i64 nvs_set_i64,
}

impl Value for bool {
    type Raw = u8;
    type Arg<'a> = bool;

    fn from_raw(raw: u8) -> Option<Self> {
        Some(raw != 0)
    }

    fn to_raw<'a>(value: Self::Arg<'a>) -> <Self::Raw as Raw>::Arg<'a> {
        u8::from(value)
    }
}

impl sealed::Sealed for String {}

impl Raw for String {
    type Arg<'a> = &'a str;

    fn get(handle: sys::nvs_handle_t, key: &CStr) -> Result<Option<Self>> {
        let mut len = 0;
        if !found(unsafe { sys::nvs_get_str(handle, key.as_ptr(), core::ptr::null_mut(), &mut len) })? {
            return Ok(None);
        }
        let mut buf = vec![0u8; len];
        if !found(unsafe { sys::nvs_get_str(handle, key.as_ptr(), buf.as_mut_ptr().cast(), &mut len) })? {
            return Ok(None);
        }
        buf.truncate(len.saturating_sub(1));
        Ok(String::from_utf8(buf).ok())
    }

    fn set(handle: sys::nvs_handle_t, key: &CStr, value: &str) -> Result<()> {
        let value = CString::new(value).map_err(|_| Error::INVALID_ARG)?;
        Error::check(unsafe { sys::nvs_set_str(handle, key.as_ptr(), value.as_ptr()) })
    }
}

impl Value for String {
    type Raw = String;
    type Arg<'a> = &'a str;

    fn from_raw(raw: String) -> Option<Self> {
        Some(raw)
    }

    fn to_raw<'a>(value: Self::Arg<'a>) -> <Self::Raw as Raw>::Arg<'a> {
        value
    }
}

impl sealed::Sealed for Vec<u8> {}

impl Raw for Vec<u8> {
    type Arg<'a> = &'a [u8];

    fn get(handle: sys::nvs_handle_t, key: &CStr) -> Result<Option<Self>> {
        let mut len = 0;
        if !found(unsafe { sys::nvs_get_blob(handle, key.as_ptr(), core::ptr::null_mut(), &mut len) })? {
            return Ok(None);
        }
        let mut buf = vec![0u8; len];
        if !found(unsafe { sys::nvs_get_blob(handle, key.as_ptr(), buf.as_mut_ptr().cast(), &mut len) })? {
            return Ok(None);
        }
        buf.truncate(len);
        Ok(Some(buf))
    }

    fn set(handle: sys::nvs_handle_t, key: &CStr, value: &[u8]) -> Result<()> {
        Error::check(unsafe { sys::nvs_set_blob(handle, key.as_ptr(), value.as_ptr().cast(), value.len()) })
    }
}

impl Value for Vec<u8> {
    type Raw = Vec<u8>;
    type Arg<'a> = &'a [u8];

    fn from_raw(raw: Vec<u8>) -> Option<Self> {
        Some(raw)
    }

    fn to_raw<'a>(value: Self::Arg<'a>) -> <Self::Raw as Raw>::Arg<'a> {
        value
    }
}

/// A namespace of the default partition, open for reading and writing.
pub struct Nvs {
    handle: sys::nvs_handle_t,
}

unsafe impl Send for Nvs {}

impl Nvs {
    /// Initializes the partition if needed; the namespace is created when missing.
    pub fn open(namespace: &CStr) -> Result<Self> {
        if namespace.to_bytes().len() > MAX_NAME {
            return Err(Error::INVALID_ARG);
        }
        init()?;
        let mut handle = 0;
        Error::check(unsafe { sys::nvs_open(namespace.as_ptr(), sys::nvs_open_mode_t_NVS_READWRITE, &mut handle) })?;
        Ok(Self { handle })
    }

    /// `None` also when the stored value does not decode as `T`.
    pub fn get<T: Value>(&self, key: Key<T>) -> Result<Option<T>> {
        Ok(<T::Raw as Raw>::get(self.handle, key.name)?.and_then(T::from_raw))
    }

    /// Lasts once `commit` has returned. A string holding a NUL is `INVALID_ARG`.
    pub fn set<T: Value>(&mut self, key: Key<T>, value: T::Arg<'_>) -> Result<()> {
        <T::Raw as Raw>::set(self.handle, key.name, T::to_raw(value))
    }

    /// A missing key is not an error.
    pub fn remove<T>(&mut self, key: Key<T>) -> Result<()> {
        found(unsafe { sys::nvs_erase_key(self.handle, key.name.as_ptr()) }).map(|_| ())
    }

    pub fn erase_all(&mut self) -> Result<()> {
        Error::check(unsafe { sys::nvs_erase_all(self.handle) })
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
