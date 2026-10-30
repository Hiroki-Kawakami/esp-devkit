/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::{CStr, c_char, c_int};
use core::mem::MaybeUninit;

use crate::{Error, Result, sys};

const O_RDONLY: c_int = 0;
const SEEK_SET: c_int = 0;

// libc keeps it in a different header on each platform.
unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
}

/// A file open for reading. `seek` and `read` share one position, so tasks
/// sharing a `File` have to serialize a seek with the read after it.
#[derive(Debug)]
pub struct File {
    fd: c_int,
}

impl File {
    pub fn open(path: &CStr) -> Result<Self> {
        let fd = unsafe { open(path.as_ptr(), O_RDONLY) };
        if fd < 0 { Err(Error::FAIL) } else { Ok(Self { fd }) }
    }

    /// May return fewer bytes than asked; 0 is the end of the file.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let n = unsafe { sys::read(self.fd, buf.as_mut_ptr().cast(), buf.len() as _) };
        usize::try_from(n).map_err(|_| Error::FAIL)
    }

    pub fn seek(&self, pos: u64) -> Result<()> {
        let offset = sys::off_t::try_from(pos).map_err(|_| Error::INVALID_ARG)?;
        let at = unsafe { sys::lseek(self.fd, offset, SEEK_SET) };
        if at == offset { Ok(()) } else { Err(Error::FAIL) }
    }

    pub fn size(&self) -> Result<u64> {
        let mut info = MaybeUninit::<sys::stat>::uninit();
        if unsafe { sys::fstat(self.fd, info.as_mut_ptr()) } != 0 {
            return Err(Error::FAIL);
        }
        Ok(unsafe { info.assume_init() }.st_size as u64)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe { sys::close(self.fd) };
    }
}

#[cfg(test)]
mod tests {
    use super::File;
    use crate::Error;
    use std::ffi::CString;

    #[test]
    fn reads_seeks_and_reports_size() {
        let path = std::env::temp_dir().join(format!("idf_fs_test_{}", std::process::id()));
        std::fs::write(&path, b"hello world").unwrap();
        let name = CString::new(path.to_str().unwrap()).unwrap();

        let f = File::open(&name).unwrap();
        assert_eq!(f.size(), Ok(11));
        let mut buf = [0; 5];
        assert_eq!(f.read(&mut buf), Ok(5));
        assert_eq!(&buf, b"hello");
        f.seek(6).unwrap();
        let mut rest = [0; 16];
        assert_eq!(f.read(&mut rest), Ok(5));
        assert_eq!(&rest[..5], b"world");
        assert_eq!(f.read(&mut rest), Ok(0));

        drop(f);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(File::open(&name).unwrap_err(), Error::FAIL);
    }
}
