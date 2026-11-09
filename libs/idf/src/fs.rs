/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::ffi::CString;
use core::ffi::{CStr, c_char, c_int};
use core::mem::MaybeUninit;

use crate::{Error, Result, sys};

const O_RDONLY: c_int = 0;
const SEEK_SET: c_int = 0;
const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;
const DIR_MODE: sys::mode_t = 0o777;
const FILE_MODE: c_int = 0o666;

// libc keeps these in a different header on each platform.
unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    #[link_name = "rename"]
    fn libc_rename(from: *const c_char, to: *const c_char) -> c_int;
}

#[cfg(target_os = "none")]
mod flags {
    use core::ffi::c_int;
    pub(super) const O_WRONLY: c_int = 1;
    pub(super) const O_RDWR: c_int = 2;
    pub(super) const O_APPEND: c_int = 0x0008;
    pub(super) const O_CREAT: c_int = 0x0200;
    pub(super) const O_TRUNC: c_int = 0x0400;
}

#[cfg(target_os = "macos")]
mod flags {
    use core::ffi::c_int;
    pub(super) const O_WRONLY: c_int = 1;
    pub(super) const O_RDWR: c_int = 2;
    pub(super) const O_APPEND: c_int = 0x0008;
    pub(super) const O_CREAT: c_int = 0x0200;
    pub(super) const O_TRUNC: c_int = 0x0400;
}

#[cfg(target_os = "linux")]
mod flags {
    use core::ffi::c_int;
    pub(super) const O_WRONLY: c_int = 1;
    pub(super) const O_RDWR: c_int = 2;
    pub(super) const O_APPEND: c_int = 0o2000;
    pub(super) const O_CREAT: c_int = 0o100;
    pub(super) const O_TRUNC: c_int = 0o1000;
}

#[cfg(target_os = "macos")]
fn mtime(info: &sys::stat) -> i64 {
    info.st_mtimespec.tv_sec as i64
}

#[cfg(not(target_os = "macos"))]
fn mtime(info: &sys::stat) -> i64 {
    info.st_mtim.tv_sec as i64
}

fn check(ret: c_int) -> Result<()> {
    if ret == 0 { Ok(()) } else { Err(Error::FAIL) }
}

/// How `OpenOptions::open` opens a file, as `std::fs::OpenOptions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    truncate: bool,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    pub fn open(&self, path: &CStr) -> Result<File> {
        let writes = self.write || self.append;
        let mut flags = match (self.read, writes) {
            (true, true) => flags::O_RDWR,
            (false, true) => flags::O_WRONLY,
            (_, false) => O_RDONLY,
        };
        if self.append {
            flags |= flags::O_APPEND;
        }
        if self.create {
            flags |= flags::O_CREAT;
        }
        if self.truncate {
            flags |= flags::O_TRUNC;
        }
        let fd = unsafe { open(path.as_ptr(), flags, FILE_MODE) };
        if fd < 0 { Err(Error::FAIL) } else { Ok(File { fd }) }
    }
}

/// An open file. `seek`, `read` and `write` share one position, so tasks
/// sharing a `File` have to serialize a seek with the access after it.
#[derive(Debug)]
pub struct File {
    fd: c_int,
}

impl File {
    /// For reading.
    pub fn open(path: &CStr) -> Result<Self> {
        OpenOptions::new().read(true).open(path)
    }

    /// For writing, created or emptied.
    pub fn create(path: &CStr) -> Result<Self> {
        OpenOptions::new().write(true).create(true).truncate(true).open(path)
    }

    /// May return fewer bytes than asked; 0 is the end of the file.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let n = unsafe { sys::read(self.fd, buf.as_mut_ptr().cast(), buf.len() as _) };
        usize::try_from(n).map_err(|_| Error::FAIL)
    }

    /// May write fewer bytes than given.
    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        let n = unsafe { sys::write(self.fd, buf.as_ptr().cast(), buf.len() as _) };
        usize::try_from(n).map_err(|_| Error::FAIL)
    }

    pub fn write_all(&self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            match self.write(buf)? {
                0 => return Err(Error::FAIL),
                n => buf = &buf[n..],
            }
        }
        Ok(())
    }

    pub fn seek(&self, pos: u64) -> Result<()> {
        let offset = sys::off_t::try_from(pos).map_err(|_| Error::INVALID_ARG)?;
        let at = unsafe { sys::lseek(self.fd, offset, SEEK_SET) };
        if at == offset { Ok(()) } else { Err(Error::FAIL) }
    }

    pub fn size(&self) -> Result<u64> {
        let mut info = MaybeUninit::<sys::stat>::uninit();
        check(unsafe { sys::fstat(self.fd, info.as_mut_ptr()) })?;
        Ok(unsafe { info.assume_init() }.st_size as u64)
    }

    /// Writes what is buffered through to the storage.
    pub fn sync(&self) -> Result<()> {
        check(unsafe { sys::fsync(self.fd) })
    }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe { sys::close(self.fd) };
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Metadata {
    len: u64,
    dir: bool,
    modified: i64,
}

impl Metadata {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_dir(&self) -> bool {
        self.dir
    }

    pub fn is_file(&self) -> bool {
        !self.dir
    }

    /// Seconds since 1970.
    pub fn modified(&self) -> i64 {
        self.modified
    }
}

pub fn metadata(path: &CStr) -> Result<Metadata> {
    let mut info = MaybeUninit::<sys::stat>::uninit();
    check(unsafe { sys::stat(path.as_ptr(), info.as_mut_ptr()) })?;
    let info = unsafe { info.assume_init() };
    Ok(Metadata { len: info.st_size as u64, dir: u32::from(info.st_mode) & S_IFMT == S_IFDIR, modified: mtime(&info) })
}

pub fn remove_file(path: &CStr) -> Result<()> {
    check(unsafe { sys::unlink(path.as_ptr()) })
}

/// The directory must be empty.
pub fn remove_dir(path: &CStr) -> Result<()> {
    check(unsafe { sys::rmdir(path.as_ptr()) })
}

pub fn create_dir(path: &CStr) -> Result<()> {
    check(unsafe { sys::mkdir(path.as_ptr(), DIR_MODE) })
}

/// Replaces `to` if it exists, on file systems that allow it.
pub fn rename(from: &CStr, to: &CStr) -> Result<()> {
    check(unsafe { libc_rename(from.as_ptr(), to.as_ptr()) })
}

/// The entries of a directory, without `.` and `..`, in no set order.
pub fn read_dir(path: &CStr) -> Result<ReadDir> {
    let dir = unsafe { sys::opendir(path.as_ptr()) };
    if dir.is_null() { Err(Error::FAIL) } else { Ok(ReadDir { dir }) }
}

pub struct ReadDir {
    dir: *mut sys::DIR,
}

unsafe impl Send for ReadDir {}

impl Iterator for ReadDir {
    type Item = DirEntry;

    fn next(&mut self) -> Option<DirEntry> {
        loop {
            let entry = unsafe { sys::readdir(self.dir) };
            if entry.is_null() {
                return None;
            }
            let entry = unsafe { &*entry };
            let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            return Some(DirEntry { name: name.into(), dir: u32::from(entry.d_type) == sys::DT_DIR });
        }
    }
}

impl Drop for ReadDir {
    fn drop(&mut self) {
        unsafe { sys::closedir(self.dir) };
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    name: CString,
    dir: bool,
}

impl DirEntry {
    pub fn file_name(&self) -> &CStr {
        &self.name
    }

    pub fn is_dir(&self) -> bool {
        self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use std::ffi::CString;

    fn temp(name: &str) -> (std::path::PathBuf, CString) {
        let path = std::env::temp_dir().join(format!("idf_fs_{name}_{}", std::process::id()));
        let c = CString::new(path.to_str().unwrap()).unwrap();
        (path, c)
    }

    #[test]
    fn reads_seeks_and_reports_size() {
        let (path, name) = temp("read");
        std::fs::write(&path, b"hello world").unwrap();

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
        assert!(f.write(b"x").is_err());

        drop(f);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(File::open(&name).unwrap_err(), Error::FAIL);
    }

    #[test]
    fn creates_writes_appends_and_renames() {
        let (path, name) = temp("write");
        let (moved, moved_name) = temp("moved");
        let f = File::create(&name).unwrap();
        f.write_all(b"hello").unwrap();
        f.sync().unwrap();
        drop(f);
        let f = OpenOptions::new().append(true).open(&name).unwrap();
        f.write_all(b" world").unwrap();
        drop(f);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");

        let f = File::create(&name).unwrap();
        f.write_all(b"new").unwrap();
        drop(f);
        assert_eq!(metadata(&name).unwrap().len(), 3);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!((metadata(&name).unwrap().modified() - now).abs() < 60);

        rename(&name, &moved_name).unwrap();
        assert!(metadata(&name).is_err());
        assert_eq!(std::fs::read(&moved).unwrap(), b"new");
        remove_file(&moved_name).unwrap();
        assert!(metadata(&moved_name).is_err());
    }

    #[test]
    fn lists_creates_and_removes_directories() {
        let (path, name) = temp("dir");
        create_dir(&name).unwrap();
        assert!(metadata(&name).unwrap().is_dir());
        std::fs::write(path.join("a.txt"), b"a").unwrap();
        std::fs::create_dir(path.join("sub")).unwrap();

        let mut entries: Vec<(Vec<u8>, bool)> =
            read_dir(&name).unwrap().map(|e| (e.file_name().to_bytes().to_vec(), e.is_dir())).collect();
        entries.sort();
        assert_eq!(entries, [(b"a.txt".to_vec(), false), (b"sub".to_vec(), true)]);

        assert!(remove_dir(&name).is_err());
        std::fs::remove_file(path.join("a.txt")).unwrap();
        std::fs::remove_dir(path.join("sub")).unwrap();
        remove_dir(&name).unwrap();
        assert!(read_dir(&name).is_err());
    }
}
