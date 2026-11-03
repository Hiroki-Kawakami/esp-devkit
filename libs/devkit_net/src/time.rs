/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ops::{Add, Sub};
use core::time::Duration;

/// Microseconds on a clock that never wraps.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Instant(u64);

impl Instant {
    pub const ZERO: Instant = Instant(0);

    pub const fn from_micros(us: u64) -> Self {
        Self(us)
    }

    pub const fn as_micros(self) -> u64 {
        self.0
    }

    /// Zero when `earlier` is later.
    pub fn saturating_duration_since(self, earlier: Instant) -> Duration {
        Duration::from_micros(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, d: Duration) -> Instant {
        Instant(self.0 + d.as_micros() as u64)
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, earlier: Instant) -> Duration {
        self.saturating_duration_since(earlier)
    }
}
