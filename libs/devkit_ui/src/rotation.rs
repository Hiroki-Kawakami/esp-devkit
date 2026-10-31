/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::time::Duration;

use bsp_rs::display::Rotation;
use bsp_rs::imu::{Imu, Orientation};
use idf::log::error;
use idf::nvs::{self, Nvs};
use idf::sync::{Mutex, Semaphore};
use idf::{Result, thread};

use crate::app::{App, AppConfig, Input};
use crate::component::Screen;

const TAG: &CStr = c"rotation";
const DEFAULT_NAMESPACE: &CStr = c"devkit_ui";
const KEY_ROTATION: &CStr = c"rotation";
const KEY_LOCKED: &CStr = c"locked";
const SAVE_DELAY: Duration = Duration::from_secs(5);
const SAVER_STACK: usize = 4096;
const SAVER_PRIORITY: u32 = 2;

devkit::bitflags! {
    /// The turns a screen can be shown at. `R0` to `R270` are the panel's own,
    /// so which of them is upright differs by board; `PORTRAIT` and `LANDSCAPE`
    /// mean the shape on any board. `CURRENT` keeps the turn the screen found.
    pub struct Rotations: u8 {
        const R0 = 1 << 0;
        const R90 = 1 << 1;
        const R180 = 1 << 2;
        const R270 = 1 << 3;
        const PORTRAIT = 1 << 4;
        const LANDSCAPE = 1 << 5;
        const CURRENT = 1 << 6;
        const ALL = 0b1111;
    }
}

impl Default for Rotations {
    fn default() -> Self {
        Self::ALL
    }
}

const TURNS: [(Rotation, Rotations); 4] =
    [(Rotation::R0, Rotations::R0), (Rotation::R90, Rotations::R90), (Rotation::R180, Rotations::R180), (Rotation::R270, Rotations::R270)];

impl Rotations {
    fn allows(self, rotation: Rotation, panel: (i16, i16)) -> bool {
        let (_, bit) = TURNS.into_iter().find(|(r, _)| *r == rotation).unwrap_or((Rotation::R0, Self::R0));
        let turned = matches!(rotation, Rotation::R90 | Rotation::R270);
        let tall = (panel.1 >= panel.0) != turned;
        self.contains(bit) || self.contains(Self::PORTRAIT) && tall || self.contains(Self::LANDSCAPE) && !tall
    }

    /// What to show, given what was asked for and what shows now.
    pub(crate) fn pick(self, requested: Rotation, current: Rotation, panel: (i16, i16)) -> Rotation {
        if self.contains(Self::CURRENT) {
            return current;
        }
        if self.allows(requested, panel) {
            return requested;
        }
        if self.allows(current, panel) {
            return current;
        }
        TURNS.into_iter().map(|(r, _)| r).find(|r| self.allows(*r, panel)).unwrap_or(current)
    }
}

fn code(rotation: Rotation) -> u8 {
    match rotation {
        Rotation::R0 => 0,
        Rotation::R90 => 1,
        Rotation::R180 => 2,
        Rotation::R270 => 3,
    }
}

fn from_code(code: u8) -> Rotation {
    match code {
        1 => Rotation::R90,
        2 => Rotation::R180,
        3 => Rotation::R270,
        _ => Rotation::R0,
    }
}

struct State {
    rotation: Rotation,
    locked: bool,
    input: Option<Input>,
    changes: u32,
}

struct Inner {
    imu: Option<&'static Imu>,
    nvs: Mutex<Nvs>,
    state: Mutex<State>,
    save: Semaphore,
}

/// Follows the IMU and remembers the turn and the lock in NVS. The whole of it
/// is `Input::set_rotation`; an app wanting something else calls that itself.
#[derive(Clone)]
pub struct AutoRotation(Arc<Inner>);

impl AutoRotation {
    pub fn new(imu: Option<&'static Imu>) -> Result<Self> {
        Self::with_namespace(imu, DEFAULT_NAMESPACE)
    }

    /// Keeps the keys `rotation` and `locked` in `namespace`, of 15 bytes at most.
    pub fn with_namespace(imu: Option<&'static Imu>, namespace: &CStr) -> Result<Self> {
        nvs::init()?;
        let nvs = Nvs::open(namespace)?;
        let rotation = nvs.get_u8(KEY_ROTATION)?.map_or(Rotation::R0, from_code);
        let locked = nvs.get_u8(KEY_LOCKED)? == Some(1);
        Ok(Self(Arc::new(Inner {
            imu,
            nvs: Mutex::try_new(nvs)?,
            state: Mutex::try_new(State { rotation, locked, input: None, changes: 0 })?,
            save: Semaphore::try_new(0, 1)?,
        })))
    }

    /// The turn it remembers and has asked for.
    pub fn rotation(&self) -> Rotation {
        self.0.state.lock().rotation
    }

    pub fn locked(&self) -> bool {
        self.0.state.lock().locked
    }

    /// Takes the IMU's pose callback: an app that also wants poses calls
    /// `Input::set_rotation` from its own instead of using this type.
    pub fn start<C: Screen>(&self, app: &App<C>) -> Result<()> {
        self.0.state.lock().input = Some(app.input());
        let saver = self.0.clone();
        thread::Builder::new()
            .name("rotation")
            .stack_size(SAVER_STACK)
            .priority(SAVER_PRIORITY)
            .spawn(move || saver.save_when_settled())?;
        if let Some(imu) = self.0.imu {
            let inner = self.0.clone();
            imu.set_orientation_cb(Some(Box::new(move |pose| inner.pose(pose))));
            imu.set_orientation_enabled(!self.locked());
        }
        Ok(())
    }

    /// While locked the turn stays as it is and the IMU rests.
    pub fn set_locked(&self, locked: bool) {
        self.0.state.lock().locked = locked;
        if let Some(imu) = self.0.imu {
            imu.set_orientation_enabled(!locked);
        }
        self.0.save_now();
    }
}

impl Inner {
    fn pose(&self, pose: Orientation) {
        let Orientation::Rotated(rotation) = pose else { return };
        let mut state = self.state.lock();
        if state.locked || state.rotation == rotation {
            return;
        }
        state.rotation = rotation;
        state.changes = state.changes.wrapping_add(1);
        if let Some(input) = &state.input {
            input.set_rotation(rotation);
        }
        drop(state);
        self.save.give();
    }

    fn save_when_settled(&self) {
        loop {
            self.save.take();
            loop {
                let changes = self.state.lock().changes;
                thread::sleep(SAVE_DELAY);
                if self.state.lock().changes == changes {
                    break;
                }
            }
            self.save_now();
        }
    }

    fn save_now(&self) {
        let (rotation, locked) = {
            let state = self.state.lock();
            (state.rotation, state.locked)
        };
        let mut nvs = self.nvs.lock();
        let saved = nvs.set_u8(KEY_ROTATION, code(rotation)).and_then(|()| nvs.set_u8(KEY_LOCKED, u8::from(locked))).and_then(|()| nvs.commit());
        if let Err(err) = saved {
            error!(TAG, "save: {}", err.to_string());
        }
    }
}

impl AppConfig {
    /// Starts at the turn `rotation` remembers.
    pub fn apply(&mut self, rotation: &AutoRotation) {
        self.presenter.rotation = rotation.rotation();
    }
}
