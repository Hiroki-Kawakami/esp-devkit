/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;

use idf::Result;
use idf::sync::Mutex;

use super::Host;
use crate::imu::{self, AxisMap, Driver, Imu, Reading, Settings, Vec3};

const AXIS_MAP: AxisMap = AxisMap { x: 1, y: 2, z: 3 };
const INITIAL: Settings = Settings { odr_hz: 100, accel_range_g: 8, gyro_range_dps: 2000 };
const UPRIGHT: Vec3 = Vec3 { x: 0.0, y: -1.0, z: 0.0 };

struct SimImu {
    latest: Arc<Mutex<(Vec3, Vec3)>>,
    settings: Settings,
}

impl Driver for SimImu {
    fn settings(&self) -> Settings {
        self.settings
    }

    fn read(&mut self) -> Result<Reading> {
        let (accel, gyro) = *self.latest.lock();
        Ok(Reading { accel, gyro, updated: true })
    }

    fn configure(&mut self, settings: &Settings) -> Result<Settings> {
        self.settings = *settings;
        Ok(*settings)
    }
}

pub(crate) fn start(host: &'static Host) -> Result<&'static Imu> {
    let latest = Arc::new(Mutex::try_new((UPRIGHT, Vec3::default()))?);
    let listener = latest.clone();
    host.set_imu_listener(Box::new(move |[ax, ay, az], [gx, gy, gz]| {
        *listener.lock() = (Vec3 { x: ax, y: ay, z: az }, Vec3 { x: gx, y: gy, z: gz });
    }));
    imu::start(SimImu { latest, settings: INITIAL }, AXIS_MAP)
}
