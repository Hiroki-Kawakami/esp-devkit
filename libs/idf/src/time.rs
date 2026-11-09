/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
#[cfg(target_os = "none")]
use core::ffi::{c_char, c_int};
use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::sync::atomic::{AtomicI32, Ordering};
#[cfg(not(target_os = "none"))]
use core::sync::atomic::AtomicI64;
use core::time::Duration;

use crate::{Error, Result, sys};

#[cfg(target_os = "none")]
unsafe extern "C" {
    fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
    fn tzset();
}

const MAX_UTC_OFFSET: i32 = 14 * 60;

static UTC_OFFSET: AtomicI32 = AtomicI32::new(0);
// The host clock stays as it is; the simulator's wall clock is an offset on it.
#[cfg(not(target_os = "none"))]
static WALL_OFFSET: AtomicI64 = AtomicI64::new(0);

/// Seconds since 1970 UTC. Until `set_unix_time`, it counts from boot.
pub fn unix_time() -> i64 {
    #[cfg(target_os = "none")]
    {
        let mut tv = sys::timeval { tv_sec: 0, tv_usec: 0 };
        unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
        tv.tv_sec as i64
    }
    #[cfg(not(target_os = "none"))]
    {
        Instant::now().since_boot().as_secs() as i64 + WALL_OFFSET.load(Ordering::Relaxed)
    }
}

pub fn set_unix_time(seconds: i64) -> Result<()> {
    #[cfg(target_os = "none")]
    {
        let tv = sys::timeval { tv_sec: seconds as _, tv_usec: 0 };
        if unsafe { sys::settimeofday(&tv, core::ptr::null()) } != 0 {
            return Err(Error::FAIL);
        }
    }
    #[cfg(not(target_os = "none"))]
    WALL_OFFSET.store(seconds - Instant::now().since_boot().as_secs() as i64, Ordering::Relaxed);
    Ok(())
}

/// Minutes east of UTC, for local time and the times files get.
pub fn utc_offset() -> i32 {
    UTC_OFFSET.load(Ordering::Relaxed)
}

pub fn set_utc_offset(minutes: i32) -> Result<()> {
    if !(-MAX_UTC_OFFSET..=MAX_UTC_OFFSET).contains(&minutes) {
        return Err(Error::INVALID_ARG);
    }
    #[cfg(target_os = "none")]
    {
        let tz = posix_tz(minutes);
        if unsafe { setenv(c"TZ".as_ptr(), tz.as_ptr().cast(), 1) } != 0 {
            return Err(Error::NO_MEM);
        }
        unsafe { tzset() };
    }
    UTC_OFFSET.store(minutes, Ordering::Relaxed);
    Ok(())
}

/// A NUL-terminated POSIX `TZ` for a fixed offset, whose sign is the opposite
/// of the offset's.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
fn posix_tz(minutes: i32) -> String {
    let sign = if minutes > 0 { '-' } else { '+' };
    let (hours, rest) = (minutes.abs() / 60, minutes.abs() % 60);
    if rest == 0 { alloc::format!("UTC{sign}{hours}\0") } else { alloc::format!("UTC{sign}{hours}:{rest:02}\0") }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Instant(i64);

impl Instant {
    pub fn now() -> Self {
        Self(unsafe { sys::esp_timer_get_time() })
    }

    pub fn elapsed(&self) -> Duration {
        Self::now().duration_since(*self)
    }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.checked_duration_since(earlier).unwrap_or(Duration::ZERO)
    }

    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        let us = self.0.checked_sub(earlier.0)?;
        u64::try_from(us).ok().map(Duration::from_micros)
    }

    pub fn checked_add(&self, d: Duration) -> Option<Instant> {
        self.0.checked_add(i64::try_from(d.as_micros()).ok()?).map(Self)
    }

    pub fn checked_sub(&self, d: Duration) -> Option<Instant> {
        self.0.checked_sub(i64::try_from(d.as_micros()).ok()?).map(Self)
    }

    pub fn since_boot(&self) -> Duration {
        Duration::from_micros(self.0.max(0) as u64)
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant {
        self.checked_add(d).expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, d: Duration) {
        *self = *self + d;
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;
    fn sub(self, d: Duration) -> Instant {
        self.checked_sub(d).expect("overflow when subtracting duration from instant")
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, d: Duration) {
        *self = *self - d;
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, earlier: Instant) -> Duration {
        self.duration_since(earlier)
    }
}

#[cfg(test)]
mod tests {
    use super::Instant;
    use core::time::Duration;

    #[test]
    fn arithmetic() {
        let a = Instant(1_000);
        let b = a + Duration::from_millis(2);
        assert_eq!(b, Instant(3_000));
        assert_eq!(b - a, Duration::from_millis(2));
        assert_eq!(a - b, Duration::ZERO);
        assert_eq!(a.checked_duration_since(b), None);
        assert_eq!(b - Duration::from_micros(500), Instant(2_500));
        let mut c = a;
        c += Duration::from_micros(5);
        c -= Duration::from_micros(2);
        assert_eq!(c, Instant(1_003));
        assert!(a < b);
    }

    #[test]
    fn overflow() {
        assert_eq!(Instant(i64::MAX).checked_add(Duration::from_micros(1)), None);
        assert_eq!(Instant(0).checked_add(Duration::MAX), None);
        assert_eq!(Instant(i64::MIN).checked_sub(Duration::from_micros(1)), None);
        assert_eq!(Instant(-5).since_boot(), Duration::ZERO);
        assert_eq!(Instant(1_500).since_boot(), Duration::from_micros(1_500));
    }

    #[test]
    fn posix_tz_inverts_the_sign() {
        assert_eq!(super::posix_tz(540), "UTC-9\0");
        assert_eq!(super::posix_tz(-300), "UTC+5\0");
        assert_eq!(super::posix_tz(330), "UTC-5:30\0");
        assert_eq!(super::posix_tz(0), "UTC+0\0");
    }

    #[test]
    fn utc_offset_range() {
        assert!(super::set_utc_offset(15 * 60).is_err());
        assert!(super::set_utc_offset(-15 * 60).is_err());
        assert_eq!(super::utc_offset(), 0);
    }

    #[test]
    fn set_unix_time_moves_the_wall_clock() {
        super::set_unix_time(1_800_000_000).unwrap();
        let now = super::unix_time();
        assert!((1_800_000_000..1_800_000_002).contains(&now));
    }

    #[test]
    fn now_advances() {
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        assert!(start.elapsed() >= Duration::from_millis(2));
        assert!(Instant::now() > start);
    }
}
