/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use core::ffi::{CStr, c_int};
use core::fmt::{self, Write};

use devkit::FmtBuf;

use crate::sys;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Verbose = 5,
}

#[cfg(target_os = "none")]
const MAX_LEVEL: u32 = sdkconfig::CONFIG_LOG_MAXIMUM_LEVEL as u32;
// The simulator's Kconfig has no log options; idf_compat prints up to Info.
#[cfg(not(target_os = "none"))]
const MAX_LEVEL: u32 = Level::Info as u32;

#[inline(always)]
pub fn enabled(level: Level, tag: &CStr) -> bool {
    level as u32 <= MAX_LEVEL
        && unsafe { sys::esp_log_level_get(tag.as_ptr()) } >= level as sys::esp_log_level_t
}

#[inline(never)]
pub fn write(level: Level, tag: &CStr, args: fmt::Arguments<'_>) {
    let mut buf = FmtBuf::<256>::new();
    if buf.write_fmt(args).is_ok() {
        output(level, tag, buf.as_bytes());
    } else {
        let mut line = String::new();
        let _ = line.write_fmt(args);
        output(level, tag, line.as_bytes());
    }
}

// Options that depend on another option are absent from sdkconfig, and so
// unknown to --check-cfg, when that option is off.
#[allow(unexpected_cfgs)]
const _: () = {
    #[cfg(CONFIG_LOG_MODE_BINARY_EN)]
    compile_error!("idf::log does not support CONFIG_LOG_MODE_BINARY_EN");
};

#[allow(unexpected_cfgs)]
#[cfg(not(CONFIG_LOG_VERSION_2))]
fn output(level: Level, tag: &CStr, msg: &[u8]) {
    let (color, reset) = colors(level, cfg!(CONFIG_LOG_COLORS));
    let level_t = level as sys::esp_log_level_t;
    let letter = letter(level) as c_int;
    let (len, msg) = (msg.len() as c_int, msg.as_ptr());
    #[cfg(not(CONFIG_LOG_TIMESTAMP_SOURCE_SYSTEM))]
    unsafe {
        let ts = sys::esp_log_timestamp() as core::ffi::c_ulong;
        let format = c"%s%c (%lu) %s: %.*s%s\n";
        sys::esp_log_write(level_t, tag.as_ptr(), format.as_ptr(), color.as_ptr(), letter, ts,
                           tag.as_ptr(), len, msg, reset.as_ptr());
    }
    #[cfg(CONFIG_LOG_TIMESTAMP_SOURCE_SYSTEM)]
    unsafe {
        let ts = sys::esp_log_system_timestamp();
        let format = c"%s%c (%s) %s: %.*s%s\n";
        sys::esp_log_write(level_t, tag.as_ptr(), format.as_ptr(), color.as_ptr(), letter, ts,
                           tag.as_ptr(), len, msg, reset.as_ptr());
    }
}

#[allow(unexpected_cfgs)]
#[cfg(CONFIG_LOG_VERSION_2)]
fn output(level: Level, tag: &CStr, msg: &[u8]) {
    let mut config: sys::esp_log_config_t = unsafe { core::mem::zeroed() };
    config.__bindgen_anon_1.data = level as u32
        | sys::ESP_LOG_CONFIG_REQUIRE_FORMATTING
        | if cfg!(CONFIG_LOG_COLORS) { 0 } else { sys::ESP_LOG_CONFIG_DIS_COLOR }
        | if cfg!(CONFIG_LOG_TIMESTAMP_SOURCE_NONE) { sys::ESP_LOG_CONFIG_DIS_TIMESTAMP } else { 0 };
    unsafe {
        sys::esp_log(config, tag.as_ptr(), c"%.*s".as_ptr(), msg.len() as c_int, msg.as_ptr());
    }
}

#[cfg_attr(CONFIG_LOG_VERSION_2, allow(dead_code))]
const fn letter(level: Level) -> u8 {
    match level {
        Level::Error => b'E',
        Level::Warn => b'W',
        Level::Info => b'I',
        Level::Debug => b'D',
        Level::Verbose => b'V',
    }
}

#[cfg_attr(CONFIG_LOG_VERSION_2, allow(dead_code))]
const fn colors(level: Level, enabled: bool) -> (&'static CStr, &'static CStr) {
    if !enabled {
        return (c"", c"");
    }
    let color = match level {
        Level::Error => c"\x1b[0;31m",
        Level::Warn => c"\x1b[0;33m",
        Level::Info => c"\x1b[0;32m",
        Level::Debug | Level::Verbose => c"",
    };
    (color, c"\x1b[0m")
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log {
    ($level:expr, $tag:expr, $($arg:tt)+) => {{
        let level = $level;
        let tag: &::core::ffi::CStr = $tag;
        if $crate::log::enabled(level, tag) {
            $crate::log::write(level, tag, ::core::format_args!($($arg)+));
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_error {
    ($tag:expr, $($arg:tt)+) => { $crate::__log!($crate::log::Level::Error, $tag, $($arg)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_warn {
    ($tag:expr, $($arg:tt)+) => { $crate::__log!($crate::log::Level::Warn, $tag, $($arg)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_info {
    ($tag:expr, $($arg:tt)+) => { $crate::__log!($crate::log::Level::Info, $tag, $($arg)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_debug {
    ($tag:expr, $($arg:tt)+) => { $crate::__log!($crate::log::Level::Debug, $tag, $($arg)+) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log_verbose {
    ($tag:expr, $($arg:tt)+) => { $crate::__log!($crate::log::Level::Verbose, $tag, $($arg)+) };
}

pub use crate::{
    __log_debug as debug, __log_error as error, __log_info as info, __log_verbose as verbose,
    __log_warn as warn,
};

#[cfg(test)]
mod tests {
    use super::{Level, colors, enabled, letter};
    use std::cell::Cell;

    const TAG: &core::ffi::CStr = c"idf_test";

    #[test]
    fn prefix_parts() {
        assert_eq!(letter(Level::Warn), b'W');
        assert_eq!(colors(Level::Error, false), (c"", c""));
        assert_eq!(colors(Level::Error, true), (c"\x1b[0;31m", c"\x1b[0m"));
        assert_eq!(colors(Level::Info, true), (c"\x1b[0;32m", c"\x1b[0m"));
        assert_eq!(colors(Level::Debug, true), (c"", c"\x1b[0m"));
    }

    #[test]
    fn levels_above_info_are_off_on_the_simulator() {
        assert!(enabled(Level::Error, TAG));
        assert!(enabled(Level::Info, TAG));
        assert!(!enabled(Level::Debug, TAG));
    }

    #[test]
    fn disabled_logs_do_not_evaluate_arguments() {
        let evaluated = Cell::new(0);
        let count = || {
            evaluated.set(evaluated.get() + 1);
            0
        };
        crate::log::debug!(TAG, "{}", count());
        assert_eq!(evaluated.get(), 0);
        crate::log::info!(TAG, "short line {}", count());
        crate::log::warn!(TAG, "long line {}", "x".repeat(400) + &count().to_string());
        assert_eq!(evaluated.get(), 2);
    }

    #[test]
    fn captures_caller_variables() {
        let value = 42;
        crate::log::error!(TAG, "value={value}");
    }
}
