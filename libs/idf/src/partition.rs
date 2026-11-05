/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use crate::{Error, Result, sys};

/// A partition from the partition table.
#[derive(Clone, Copy)]
pub struct Partition(&'static sys::esp_partition_t);

unsafe impl Send for Partition {}
unsafe impl Sync for Partition {}

impl Partition {
    pub fn find(label: &CStr) -> Option<Partition> {
        let p = unsafe {
            sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_ANY,
                sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
                label.as_ptr(),
            )
        };
        unsafe { p.as_ref() }.map(Partition)
    }

    /// Creates the partition the simulator keeps in `path`, from `image` when
    /// the file does not exist yet.
    #[cfg(not(target_os = "none"))]
    pub fn sim_add(label: &CStr, size: usize, path: &CStr, image: Option<&CStr>) -> Result<()> {
        Error::check(unsafe {
            sys::esp_partition_sim_add(label.as_ptr(), size, path.as_ptr(), image.map_or(core::ptr::null(), |i| i.as_ptr()))
        })
    }

    pub fn size(&self) -> usize {
        self.0.size as usize
    }

    pub fn erase_size(&self) -> usize {
        self.0.erase_size as usize
    }

    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<()> {
        Error::check(unsafe { sys::esp_partition_read(self.0, offset, buf.as_mut_ptr().cast(), buf.len()) })
    }

    /// Writing only clears bits; the range has to be erased first.
    pub fn write(&self, offset: usize, data: &[u8]) -> Result<()> {
        Error::check(unsafe { sys::esp_partition_write(self.0, offset, data.as_ptr().cast(), data.len()) })
    }

    /// `offset` and `len` are multiples of `erase_size()`.
    pub fn erase(&self, offset: usize, len: usize) -> Result<()> {
        Error::check(unsafe { sys::esp_partition_erase_range(self.0, offset, len) })
    }

    pub fn map(&self, offset: usize, len: usize) -> Result<Mapping> {
        let mut ptr = core::ptr::null();
        let mut handle = 0;
        Error::check(unsafe {
            sys::esp_partition_mmap(
                self.0,
                offset,
                len,
                sys::esp_partition_mmap_flag_t_ESP_PARTITION_MMAP_DATA,
                &mut ptr,
                &mut handle,
            )
        })?;
        Ok(Mapping { ptr: ptr.cast(), len, handle })
    }
}

/// A read-only view of partition data, unmapped on drop.
pub struct Mapping {
    ptr: *const u8,
    len: usize,
    handle: sys::esp_partition_mmap_handle_t,
}

unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { sys::esp_partition_munmap(self.handle) };
    }
}
