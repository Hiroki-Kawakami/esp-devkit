/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::time::Duration;

use devkit::math::{atan2, lroundf, sqrtf};
use idf::log::warn;
use idf::sync::{Mutex, OnceLock};
use idf::time::Instant;
use idf::{Error, Result};

use crate::dispatch::{self, Next, Notifier, Source};
use crate::display::Rotation;

const TAG: &CStr = c"imu";

const ORIENTATION_INTERVAL: Duration = Duration::from_millis(100);
const ORIENTATION_STABLE: Duration = Duration::from_millis(300);
const MIN_G: f32 = 0.5;
const MAX_G: f32 = 1.5;
// Cosines of the 35 and 45 degree tilts that enter and leave a face pose.
const FACE_ENTER_COS: f32 = 0.81915;
const FACE_EXIT_COS: f32 = 0.70711;
const ROTATION_HYSTERESIS_DEG: f32 = 15.0;

/// Panel axes: +X right, +Y down, +Z into the screen.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sample {
    /// In g, +1 pointing up at rest.
    pub accel: Vec3,
    /// In degrees per second.
    pub gyro: Vec3,
    pub timestamp: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub odr_hz: u16,
    pub accel_range_g: u16,
    pub gyro_range_dps: u16,
}

/// `None` keeps the current value. The chip rounds up to what it supports.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Request {
    pub odr_hz: Option<u16>,
    pub accel_range_g: Option<u16>,
    pub gyro_range_dps: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    /// Upright when the picture is turned by this much.
    Rotated(Rotation),
    FaceUp,
    FaceDown,
}

impl Orientation {
    const fn code(self) -> u8 {
        match self {
            Self::Rotated(Rotation::R0) => 1,
            Self::Rotated(Rotation::R90) => 2,
            Self::Rotated(Rotation::R180) => 3,
            Self::Rotated(Rotation::R270) => 4,
            Self::FaceUp => 5,
            Self::FaceDown => 6,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Rotated(Rotation::R0),
            2 => Self::Rotated(Rotation::R90),
            3 => Self::Rotated(Rotation::R180),
            4 => Self::Rotated(Rotation::R270),
            5 => Self::FaceUp,
            6 => Self::FaceDown,
            _ => return None,
        })
    }

    fn rotation_deg(self) -> Option<f32> {
        match self {
            Self::Rotated(Rotation::R0) => Some(0.0),
            Self::Rotated(Rotation::R90) => Some(90.0),
            Self::Rotated(Rotation::R180) => Some(180.0),
            Self::Rotated(Rotation::R270) => Some(270.0),
            Self::FaceUp | Self::FaceDown => None,
        }
    }
}

pub type OrientationCallback = Box<dyn FnMut(Orientation) + Send>;

/// Each output axis as a 1-based chip axis, negative to invert it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AxisMap {
    pub x: i8,
    pub y: i8,
    pub z: i8,
}

impl AxisMap {
    fn apply(&self, v: Vec3) -> Vec3 {
        let axis = |axis: i8| {
            let value = match axis.unsigned_abs() {
                1 => v.x,
                2 => v.y,
                3 => v.z,
                _ => 0.0,
            };
            if axis < 0 { -value } else { value }
        };
        Vec3 { x: axis(self.x), y: axis(self.y), z: axis(self.z) }
    }
}

pub(crate) struct Reading {
    pub accel: Vec3,
    pub gyro: Vec3,
    /// New data since the last read.
    pub updated: bool,
}

pub(crate) trait Driver: Send + 'static {
    fn settings(&self) -> Settings;
    fn read(&mut self) -> Result<Reading>;
    /// Returns what the chip took.
    fn configure(&mut self, settings: &Settings) -> Result<Settings>;
}

fn classify(a: Vec3, current: Option<Orientation>) -> Option<Orientation> {
    let norm = sqrtf(a.x * a.x + a.y * a.y + a.z * a.z);
    if !(MIN_G..=MAX_G).contains(&norm) {
        return None;
    }

    let face = matches!(current, Some(Orientation::FaceUp | Orientation::FaceDown));
    if a.z.abs() / norm > if face { FACE_EXIT_COS } else { FACE_ENTER_COS } {
        return Some(if a.z < 0.0 { Orientation::FaceUp } else { Orientation::FaceDown });
    }

    let up = atan2(f64::from(-a.x), f64::from(-a.y)).to_degrees() as f32;
    let up = if up < 0.0 { up + 360.0 } else { up };
    if let Some(deg) = current.and_then(Orientation::rotation_deg) {
        let d = (up - deg).abs();
        if (if d > 180.0 { 360.0 - d } else { d }) <= 45.0 + ROTATION_HYSTERESIS_DEG {
            return current;
        }
    }
    Some(Orientation::Rotated(match lroundf(up / 90.0) % 4 {
        0 => Rotation::R0,
        1 => Rotation::R90,
        2 => Rotation::R180,
        _ => Rotation::R270,
    }))
}

struct Inner {
    driver: Box<dyn Driver>,
    axis_map: AxisMap,
    last: Option<Sample>,
    injected: Option<Sample>,
    candidate: Option<Orientation>,
    candidate_since: Instant,
}

impl Inner {
    fn refresh(&mut self) -> Result<Sample> {
        if let Some(sample) = self.injected {
            let sample = Sample { timestamp: Instant::now(), ..sample };
            self.last = Some(sample);
            return Ok(sample);
        }
        let reading = self.driver.read()?;
        match self.last {
            Some(last) if !reading.updated => Ok(last),
            _ => {
                let sample = Sample {
                    accel: self.axis_map.apply(reading.accel),
                    gyro: self.axis_map.apply(reading.gyro),
                    timestamp: Instant::now(),
                };
                self.last = Some(sample);
                Ok(sample)
            }
        }
    }
}

/// Shared by every task.
pub struct Imu {
    inner: Mutex<Inner>,
    enabled: AtomicBool,
    orientation: AtomicU8,
    callback: Mutex<Option<Option<OrientationCallback>>>,
    callback_changed: AtomicBool,
    notifier: OnceLock<Notifier>,
}

impl Imu {
    fn new(driver: Box<dyn Driver>, axis_map: AxisMap) -> Result<Self> {
        Ok(Self {
            inner: Mutex::try_new(Inner {
                driver,
                axis_map,
                last: None,
                injected: None,
                candidate: None,
                candidate_since: Instant::now(),
            })?,
            enabled: AtomicBool::new(false),
            orientation: AtomicU8::new(0),
            callback: Mutex::try_new(None)?,
            callback_changed: AtomicBool::new(false),
            notifier: OnceLock::new(),
        })
    }

    /// The newest sample; its timestamp stays put until the chip has a new one.
    pub fn read(&self) -> Result<Sample> {
        self.inner.lock().refresh()
    }

    pub fn settings(&self) -> Settings {
        self.inner.lock().driver.settings()
    }

    /// Returns what the chip settled on.
    pub fn configure(&self, request: &Request) -> Result<Settings> {
        let mut inner = self.inner.lock();
        let current = inner.driver.settings();
        inner.driver.configure(&Settings {
            odr_hz: request.odr_hz.unwrap_or(current.odr_hz),
            accel_range_g: request.accel_range_g.unwrap_or(current.accel_range_g),
            gyro_range_dps: request.gyro_range_dps.unwrap_or(current.gyro_range_dps),
        })
    }

    /// Tracking starts over from no orientation each time it is enabled.
    pub fn set_orientation_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        self.orientation.store(0, Ordering::Relaxed);
        self.inner.lock().candidate = None;
        if let Some(notifier) = self.notifier.get() {
            notifier.notify();
        }
    }

    pub fn orientation_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// The pose settled on, `None` until tracking has one.
    pub fn orientation(&self) -> Option<Orientation> {
        Orientation::from_code(self.orientation.load(Ordering::Relaxed))
    }

    /// Runs on the dispatch task with each new pose. Takes effect from the next
    /// tick, so the previous callback may still run once after this returns.
    pub fn set_orientation_cb(&self, callback: Option<OrientationCallback>) {
        *self.callback.lock() = Some(callback);
        self.callback_changed.store(true, Ordering::Release);
        if let Some(notifier) = self.notifier.get() {
            notifier.notify();
        }
    }

    /// Replaces the sensor with `accel` and `gyro`, in panel axes, until `None`.
    pub fn inject(&self, values: Option<(Vec3, Vec3)>) {
        let mut inner = self.inner.lock();
        inner.injected = values.map(|(accel, gyro)| Sample { accel, gyro, timestamp: Instant::now() });
        if values.is_none() {
            inner.last = None;
        }
    }

    fn track(&self) -> Option<Orientation> {
        let mut inner = self.inner.lock();
        let current = self.orientation();
        let next = match inner.refresh() {
            Ok(sample) => classify(sample.accel, current),
            Err(err) => {
                warn!(TAG, "read: {err}");
                None
            }
        };
        let now = Instant::now();
        if next != inner.candidate {
            inner.candidate = next;
            inner.candidate_since = now;
        }
        let settled = inner.candidate.filter(|_| now.duration_since(inner.candidate_since) >= ORIENTATION_STABLE);
        let changed = settled.filter(|&pose| Some(pose) != current)?;
        self.orientation.store(changed.code(), Ordering::Relaxed);
        Some(changed)
    }
}

struct OrientationSource<'a> {
    imu: &'a Imu,
    callback: Option<OrientationCallback>,
}

impl OrientationSource<'_> {
    fn poll(&mut self) -> Next {
        let imu = self.imu;
        if imu.callback_changed.swap(false, Ordering::Acquire)
            && let Some(callback) = imu.callback.lock().take()
        {
            self.callback = callback;
        }
        if !imu.orientation_enabled() {
            return Next::Idle;
        }
        if let Some(pose) = imu.track()
            && let Some(callback) = &mut self.callback
        {
            callback(pose);
        }
        Next::After(ORIENTATION_INTERVAL)
    }
}

impl Source for OrientationSource<'static> {
    fn tick(&mut self) -> Next {
        self.poll()
    }
}

static IMU: OnceLock<Imu> = OnceLock::new();

pub(crate) fn start(driver: impl Driver, axis_map: AxisMap) -> Result<&'static Imu> {
    let imu = IMU.get_or_try_init(|| Imu::new(Box::new(driver), axis_map))?;
    let notifier = dispatch::add(Box::new(OrientationSource { imu, callback: None }))?;
    imu.notifier.set(notifier).map_err(|_| Error::INVALID_STATE)?;
    Ok(imu)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::sync::atomic::AtomicUsize;
    use idf::thread;
    use std::sync::Mutex as StdMutex;

    const IDENTITY: AxisMap = AxisMap { x: 1, y: 2, z: 3 };
    const INITIAL: Settings = Settings { odr_hz: 100, accel_range_g: 8, gyro_range_dps: 2000 };
    const UPRIGHT: Vec3 = Vec3 { x: 0.0, y: -1.0, z: 0.0 };

    fn rotated(rotation: Rotation) -> Option<Orientation> {
        Some(Orientation::Rotated(rotation))
    }

    #[derive(Default)]
    struct Chip {
        accel: StdMutex<Vec3>,
        updated: AtomicBool,
        reads: AtomicUsize,
        configured: StdMutex<Option<Settings>>,
    }

    struct Stub {
        chip: Arc<Chip>,
        settings: Settings,
    }

    impl Driver for Stub {
        fn settings(&self) -> Settings {
            self.settings
        }

        fn read(&mut self) -> Result<Reading> {
            self.chip.reads.fetch_add(1, Ordering::SeqCst);
            Ok(Reading {
                accel: *self.chip.accel.lock().unwrap(),
                gyro: Vec3 { x: 1.0, y: 2.0, z: 3.0 },
                updated: self.chip.updated.load(Ordering::SeqCst),
            })
        }

        fn configure(&mut self, settings: &Settings) -> Result<Settings> {
            *self.chip.configured.lock().unwrap() = Some(*settings);
            self.settings = *settings;
            Ok(*settings)
        }
    }

    fn imu(axis_map: AxisMap) -> (Imu, Arc<Chip>) {
        let chip = Arc::new(Chip::default());
        chip.updated.store(true, Ordering::SeqCst);
        *chip.accel.lock().unwrap() = UPRIGHT;
        let driver = Stub { chip: chip.clone(), settings: INITIAL };
        (Imu::new(Box::new(driver), axis_map).unwrap(), chip)
    }

    #[test]
    fn classifies_the_resting_poses() {
        let at = |x, y, z| classify(Vec3 { x, y, z }, None);
        assert_eq!(at(0.0, -1.0, 0.0), rotated(Rotation::R0));
        assert_eq!(at(-1.0, 0.0, 0.0), rotated(Rotation::R90));
        assert_eq!(at(0.0, 1.0, 0.0), rotated(Rotation::R180));
        assert_eq!(at(1.0, 0.0, 0.0), rotated(Rotation::R270));
        assert_eq!(at(0.0, 0.0, -1.0), Some(Orientation::FaceUp));
        assert_eq!(at(0.0, 0.0, 1.0), Some(Orientation::FaceDown));
    }

    #[test]
    fn free_fall_and_shocks_have_no_pose() {
        assert_eq!(classify(Vec3 { x: 0.0, y: -0.2, z: 0.0 }, None), None);
        assert_eq!(classify(Vec3 { x: 0.0, y: -2.0, z: 0.0 }, None), None);
    }

    #[test]
    fn rotation_holds_within_its_hysteresis() {
        let tilted = |deg: f32| {
            let rad = deg.to_radians();
            Vec3 { x: -rad.sin(), y: -rad.cos(), z: 0.0 }
        };
        let current = rotated(Rotation::R0);
        assert_eq!(classify(tilted(55.0), None), rotated(Rotation::R90));
        assert_eq!(classify(tilted(55.0), current), current);
        assert_eq!(classify(tilted(65.0), current), rotated(Rotation::R90));
        assert_eq!(classify(tilted(-55.0), current), current);
    }

    #[test]
    fn face_holds_until_the_exit_tilt() {
        let tilted = |deg: f32| {
            let rad = deg.to_radians();
            Vec3 { x: 0.0, y: -rad.sin(), z: -rad.cos() }
        };
        assert_eq!(classify(tilted(40.0), None), rotated(Rotation::R0));
        assert_eq!(classify(tilted(40.0), Some(Orientation::FaceUp)), Some(Orientation::FaceUp));
        assert_eq!(classify(tilted(50.0), Some(Orientation::FaceUp)), rotated(Rotation::R0));
    }

    #[test]
    fn axis_map_reorders_and_inverts() {
        let map = AxisMap { x: -1, y: 3, z: -2 };
        let v = map.apply(Vec3 { x: 1.0, y: 2.0, z: 3.0 });
        assert_eq!(v, Vec3 { x: -1.0, y: 3.0, z: -2.0 });
    }

    #[test]
    fn read_applies_the_axis_map_and_keeps_the_last_sample_without_new_data() {
        let (imu, chip) = imu(AxisMap { x: -1, y: -2, z: 3 });
        let first = imu.read().unwrap();
        assert_eq!(first.accel, Vec3 { x: 0.0, y: 1.0, z: 0.0 });
        assert_eq!(first.gyro, Vec3 { x: -1.0, y: -2.0, z: 3.0 });

        chip.updated.store(false, Ordering::SeqCst);
        *chip.accel.lock().unwrap() = Vec3 { x: 9.0, y: 9.0, z: 9.0 };
        assert_eq!(imu.read().unwrap(), first);
    }

    #[test]
    fn configure_keeps_the_unset_fields() {
        let (imu, chip) = imu(IDENTITY);
        let settings = imu.configure(&Request { odr_hz: Some(25), ..Request::default() }).unwrap();
        assert_eq!(settings, Settings { odr_hz: 25, ..INITIAL });
        assert_eq!(*chip.configured.lock().unwrap(), Some(settings));
        assert_eq!(imu.settings(), settings);
    }

    #[test]
    fn inject_replaces_the_sensor_until_released() {
        let (imu, chip) = imu(IDENTITY);
        let accel = Vec3 { x: 0.5, y: 0.0, z: 0.0 };
        imu.inject(Some((accel, Vec3::default())));
        assert_eq!(imu.read().unwrap().accel, accel);
        assert_eq!(chip.reads.load(Ordering::SeqCst), 0);

        imu.inject(None);
        assert_eq!(imu.read().unwrap().accel, UPRIGHT);
    }

    fn collect(imu: &Imu) -> (OrientationSource<'_>, Arc<StdMutex<Vec<Orientation>>>) {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = seen.clone();
        imu.set_orientation_cb(Some(Box::new(move |pose| sink.lock().unwrap().push(pose))));
        (OrientationSource { imu, callback: None }, seen)
    }

    #[test]
    fn a_pose_settles_after_it_holds() {
        let (imu, chip) = imu(IDENTITY);
        let (mut source, seen) = collect(&imu);
        assert!(matches!(source.poll(), Next::Idle));

        imu.set_orientation_enabled(true);
        assert!(matches!(source.poll(), Next::After(ORIENTATION_INTERVAL)));
        assert_eq!(imu.orientation(), None);
        thread::sleep(ORIENTATION_STABLE + Duration::from_millis(10));
        source.poll();
        assert_eq!(imu.orientation(), rotated(Rotation::R0));

        *chip.accel.lock().unwrap() = Vec3 { x: -1.0, y: 0.0, z: 0.0 };
        source.poll();
        assert_eq!(imu.orientation(), rotated(Rotation::R0));
        thread::sleep(ORIENTATION_STABLE + Duration::from_millis(10));
        source.poll();
        source.poll();
        assert_eq!(imu.orientation(), rotated(Rotation::R90));
        assert_eq!(
            *seen.lock().unwrap(),
            [Orientation::Rotated(Rotation::R0), Orientation::Rotated(Rotation::R90)]
        );
    }

    #[test]
    fn a_passing_pose_never_settles() {
        let (imu, chip) = imu(IDENTITY);
        let (mut source, seen) = collect(&imu);
        imu.set_orientation_enabled(true);
        source.poll();
        thread::sleep(ORIENTATION_STABLE / 2);
        *chip.accel.lock().unwrap() = Vec3 { x: -1.0, y: 0.0, z: 0.0 };
        source.poll();
        thread::sleep(ORIENTATION_STABLE / 2 + Duration::from_millis(10));
        source.poll();
        assert_eq!(imu.orientation(), None);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn no_pose_keeps_the_last_one() {
        let (imu, chip) = imu(IDENTITY);
        let (mut source, _seen) = collect(&imu);
        imu.set_orientation_enabled(true);
        source.poll();
        thread::sleep(ORIENTATION_STABLE + Duration::from_millis(10));
        source.poll();
        *chip.accel.lock().unwrap() = Vec3::default();
        source.poll();
        thread::sleep(ORIENTATION_STABLE + Duration::from_millis(10));
        source.poll();
        assert_eq!(imu.orientation(), rotated(Rotation::R0));
    }

    #[test]
    fn disabling_forgets_the_pose() {
        let (imu, _chip) = imu(IDENTITY);
        let (mut source, _seen) = collect(&imu);
        imu.set_orientation_enabled(true);
        source.poll();
        thread::sleep(ORIENTATION_STABLE + Duration::from_millis(10));
        source.poll();
        assert!(imu.orientation().is_some());

        imu.set_orientation_enabled(false);
        assert_eq!(imu.orientation(), None);
        assert!(matches!(source.poll(), Next::Idle));
    }
}
