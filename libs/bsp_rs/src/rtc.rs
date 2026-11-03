/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::time::Duration;

use idf::sync::{Mutex, OnceLock};
use idf::{Error, Result};

const SECONDS_PER_DAY: i64 = 86_400;

/// Local wall-clock time, without a time zone.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DateTime {
    pub year: u16,
    /// 1..=12.
    pub month: u8,
    /// 1..=31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

const fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Days since 1970-01-01 in the proleptic Gregorian calendar, after Howard
// Hinnant's days_from_civil.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl DateTime {
    pub fn is_valid(&self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=days_in_month(i64::from(self.year), self.month)).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// 0 is Sunday.
    pub fn weekday(&self) -> u8 {
        (self.days() + 4).rem_euclid(7) as u8
    }

    fn days(&self) -> i64 {
        days_from_civil(i64::from(self.year), i64::from(self.month), i64::from(self.day))
    }

    /// Seconds since 1970-01-01 00:00 on the same wall clock.
    pub fn seconds(&self) -> i64 {
        self.days() * SECONDS_PER_DAY
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    pub fn from_seconds(seconds: i64) -> Self {
        let (year, month, day) = civil_from_days(seconds.div_euclid(SECONDS_PER_DAY));
        let rest = seconds.rem_euclid(SECONDS_PER_DAY);
        Self {
            year: year.clamp(0, i64::from(u16::MAX)) as u16,
            month,
            day,
            hour: (rest / 3600) as u8,
            minute: (rest / 60 % 60) as u8,
            second: (rest % 60) as u8,
        }
    }
}

pub(crate) trait Driver: Send + 'static {
    fn now(&mut self) -> Result<DateTime>;
    /// `time` is valid.
    fn set(&mut self, time: &DateTime) -> Result<()>;
    fn is_valid(&mut self) -> Result<bool>;

    fn start_timer(&mut self, _period: Duration) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn stop_timer(&mut self) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn timer_expired(&mut self) -> Result<bool> {
        Err(Error::NOT_SUPPORTED)
    }

    fn clear_timer(&mut self) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }
}

/// A battery-backed clock, shared by every task.
pub struct Rtc(Mutex<Box<dyn Driver>>);

impl Rtc {
    pub fn now(&self) -> Result<DateTime> {
        self.0.lock().now()
    }

    /// `INVALID_ARG` for a date that does not exist or that the chip cannot
    /// hold.
    pub fn set(&self, time: &DateTime) -> Result<()> {
        if !time.is_valid() {
            return Err(Error::INVALID_ARG);
        }
        self.0.lock().set(time)
    }

    /// `false` once the clock lost power or stopped, until the next `set`.
    pub fn is_valid(&self) -> Result<bool> {
        self.0.lock().is_valid()
    }

    /// Raises the chip's interrupt and expiry flag every `period` until
    /// stopped; both survive deep sleep. A period the chip cannot represent is
    /// `INVALID_ARG`, one below the board's minimum `NOT_SUPPORTED`.
    pub fn start_timer(&self, period: Duration) -> Result<()> {
        self.0.lock().start_timer(period)
    }

    pub fn stop_timer(&self) -> Result<()> {
        self.0.lock().stop_timer()
    }

    pub fn timer_expired(&self) -> Result<bool> {
        self.0.lock().timer_expired()
    }

    pub fn clear_timer(&self) -> Result<()> {
        self.0.lock().clear_timer()
    }
}

static RTC: OnceLock<Rtc> = OnceLock::new();

pub(crate) fn start(driver: impl Driver) -> Result<&'static Rtc> {
    RTC.get_or_try_init(|| Ok(Rtc(Mutex::try_new(Box::new(driver) as Box<dyn Driver>)?)))
}

#[cfg(test)]
mod tests {
    use super::DateTime;

    const fn at(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> DateTime {
        DateTime { year, month, day, hour, minute, second }
    }

    #[test]
    fn seconds_round_trip() {
        for time in [at(1970, 1, 1, 0, 0, 0), at(2000, 2, 29, 23, 59, 59), at(2026, 11, 3, 12, 34, 56), at(2099, 12, 31, 0, 0, 1)] {
            assert_eq!(DateTime::from_seconds(time.seconds()), time);
        }
        assert_eq!(at(2000, 1, 1, 0, 0, 0).seconds(), 946_684_800);
    }

    #[test]
    fn weekday_from_the_date() {
        assert_eq!(at(1970, 1, 1, 0, 0, 0).weekday(), 4);
        assert_eq!(at(2000, 1, 1, 0, 0, 0).weekday(), 6);
        assert_eq!(at(2026, 11, 1, 0, 0, 0).weekday(), 0);
    }

    #[test]
    fn rejects_dates_that_do_not_exist() {
        assert!(at(2024, 2, 29, 0, 0, 0).is_valid());
        assert!(!at(2023, 2, 29, 0, 0, 0).is_valid());
        assert!(!at(2100, 2, 29, 0, 0, 0).is_valid());
        assert!(!at(2026, 4, 31, 0, 0, 0).is_valid());
        assert!(!at(2026, 13, 1, 0, 0, 0).is_valid());
        assert!(!at(2026, 1, 1, 24, 0, 0).is_valid());
    }
}
