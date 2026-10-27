/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

use crate::sys;

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
    fn now_advances() {
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        assert!(start.elapsed() >= Duration::from_millis(2));
        assert!(Instant::now() > start);
    }
}
