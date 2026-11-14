/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::fmt;
use core::ops::Deref;

pub struct FmtBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> FmtBuf<N> {
    const NONZERO: () = assert!(N > 0, "FmtBuf needs room for the NUL terminator");

    pub const fn new() -> Self {
        let () = Self::NONZERO;
        Self { buf: [0; N], len: 0, truncated: false }
    }

    pub fn from_fmt(args: fmt::Arguments<'_>) -> Self {
        let mut b = Self::new();
        let _ = fmt::Write::write_fmt(&mut b, args);
        b
    }

    pub fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn as_c_str(&self) -> &CStr {
        match CStr::from_bytes_until_nul(&self.buf[..=self.len]) {
            Ok(s) => s,
            Err(_) => c"",
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn capacity(&self) -> usize {
        N - 1
    }

    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
        self.buf[0] = 0;
    }
}

impl<const N: usize> fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let room = N - 1 - self.len;
        let mut n = s.len().min(room);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        self.buf[self.len] = 0;
        if n < s.len() {
            self.truncated = true;
            return Err(fmt::Error);
        }
        Ok(())
    }
}

impl<const N: usize> Default for FmtBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Deref for FmtBuf<N> {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> fmt::Display for FmtBuf<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl<const N: usize> fmt::Debug for FmtBuf<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::FmtBuf;
    use std::fmt::Write;

    #[test]
    fn writes_and_terminates() {
        let mut b = FmtBuf::<16>::new();
        assert!(b.is_empty());
        assert_eq!(b.capacity(), 15);
        assert_eq!(b.as_c_str(), c"");
        assert!(write!(b, "x={}", 42).is_ok());
        assert_eq!(b.as_str(), "x=42");
        assert_eq!(b.as_bytes(), b"x=42");
        assert_eq!(b.len(), 4);
        assert_eq!(b.as_c_str(), c"x=42");
        assert!(!b.is_truncated());
    }

    #[test]
    fn fills_exactly() {
        let mut b = FmtBuf::<5>::new();
        assert!(b.write_str("abcd").is_ok());
        assert_eq!(b.as_str(), "abcd");
        assert!(!b.is_truncated());
        assert_eq!(b.as_c_str(), c"abcd");
    }

    #[test]
    fn truncates_ascii() {
        let mut b = FmtBuf::<5>::new();
        assert!(write!(b, "{}", "abcdef").is_err());
        assert_eq!(b.as_str(), "abcd");
        assert!(b.is_truncated());
        assert_eq!(b.as_c_str(), c"abcd");
    }

    #[test]
    fn truncates_on_char_boundary() {
        let mut b = FmtBuf::<6>::new();
        assert!(b.write_str("ab\u{3042}\u{3044}").is_err());
        assert_eq!(b.as_str(), "ab\u{3042}");
        assert!(b.is_truncated());
    }

    #[test]
    fn ignores_writes_after_truncation() {
        let mut b = FmtBuf::<6>::new();
        let _ = b.write_str("abc\u{3042}");
        assert_eq!(b.as_str(), "abc");
        assert!(b.write_str("d").is_err());
        assert_eq!(b.as_str(), "abc");
    }

    #[test]
    fn clear_resets() {
        let mut b = FmtBuf::<4>::new();
        let _ = b.write_str("abcdef");
        b.clear();
        assert!(b.is_empty());
        assert!(!b.is_truncated());
        assert_eq!(b.as_c_str(), c"");
        assert!(b.write_str("xy").is_ok());
        assert_eq!(b.as_str(), "xy");
    }

    #[test]
    fn interior_nul_ends_c_str() {
        let b = FmtBuf::<16>::from_fmt(format_args!("ab\0cd"));
        assert_eq!(b.as_str(), "ab\0cd");
        assert_eq!(b.as_c_str(), c"ab");
    }

    #[test]
    fn from_fmt_and_traits() {
        let b = FmtBuf::<8>::from_fmt(format_args!("{}/{}", "root", "name"));
        assert_eq!(&*b, "root/na");
        assert!(b.is_truncated());
        let b = FmtBuf::<8>::from_fmt(format_args!("{}", 7));
        assert_eq!(format!("[{b:>3}]"), "[  7]");
        assert_eq!(format!("{b:?}"), "\"7\"");
        assert!(FmtBuf::<4>::default().is_empty());
        assert!(b.starts_with('7'));
    }

    #[test]
    fn minimal_capacity() {
        let mut b = FmtBuf::<1>::new();
        assert!(b.write_str("").is_ok());
        assert!(b.write_str("a").is_err());
        assert_eq!(b.as_c_str(), c"");
    }
}
