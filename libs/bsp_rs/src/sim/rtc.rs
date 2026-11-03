/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;

use idf::Result;
use idf::time::Instant;

use super::protocol::{ENV_RTC, Event, parse_rtc};
use super::{Host, ffi};
use crate::rtc::{DateTime, Driver};

// The host keeps the clock across restarts and starts the firmware with it.
pub(crate) struct SimRtc {
    host: &'static Host,
    base: i64,
    since: Instant,
    valid: bool,
}

impl SimRtc {
    pub(crate) fn new(host: &'static Host) -> Option<Self> {
        let value = unsafe { ffi::getenv(ENV_RTC.as_ptr()) };
        if value.is_null() {
            return None;
        }
        let (base, valid) = parse_rtc(unsafe { CStr::from_ptr(value) }.to_str().ok()?)?;
        Some(Self { host, base, since: Instant::now(), valid })
    }

    fn seconds(&self) -> i64 {
        self.base + self.since.elapsed().as_secs() as i64
    }
}

impl Driver for SimRtc {
    fn now(&mut self) -> Result<DateTime> {
        Ok(DateTime::from_seconds(self.seconds()))
    }

    fn set(&mut self, time: &DateTime) -> Result<()> {
        self.base = time.seconds();
        self.since = Instant::now();
        self.valid = true;
        self.host.send(&Event::Rtc { seconds: self.base, valid: true });
        Ok(())
    }

    fn is_valid(&mut self) -> Result<bool> {
        Ok(self.valid)
    }
}
