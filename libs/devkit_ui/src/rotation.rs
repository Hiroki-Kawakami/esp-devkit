/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;

use bsp_rs::display::Rotation;
use bsp_rs::imu::{Imu, Orientation};
use idf::nvs::Key;
use idf::sync::Mutex;

use crate::app::Input;
use crate::plugin::{Plugin, PluginContext, PluginInit, Store};

const KEY_ROTATION: Key<u8> = Key::new(c"rotation");
const KEY_LOCKED: Key<u8> = Key::new(c"locked");

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
    context: Option<(Store, Input)>,
}

struct Inner {
    imu: Option<&'static Imu>,
    state: Mutex<State>,
}

/// Follows the IMU and remembers the turn and a lock, as a `Plugin`. The whole
/// of it is `Input::set_rotation`; an app wanting something else calls that
/// itself.
#[derive(Clone)]
pub struct AutoRotation(Arc<Inner>);

impl AutoRotation {
    pub fn new(imu: Option<&'static Imu>) -> Self {
        let state = State { rotation: Rotation::R0, locked: false, context: None };
        Self(Arc::new(Inner { imu, state: Mutex::new(state) }))
    }

    /// The turn it remembers and has asked for.
    pub fn rotation(&self) -> Rotation {
        self.0.state.lock().rotation
    }

    pub fn locked(&self) -> bool {
        self.0.state.lock().locked
    }

    /// While locked the turn stays as it is and the IMU rests.
    pub fn set_locked(&self, locked: bool) {
        let mut state = self.0.state.lock();
        state.locked = locked;
        if let Some((store, _)) = &state.context {
            store.set_u8(KEY_LOCKED, u8::from(locked));
        }
        drop(state);
        if let Some(imu) = self.0.imu {
            imu.set_orientation_enabled(!locked);
        }
    }
}

impl Plugin for AutoRotation {
    fn init(&mut self, init: &mut PluginInit<'_>) {
        let saved = init.store().get_u8(KEY_ROTATION);
        let mut state = self.0.state.lock();
        state.rotation = saved.map_or(init.rotation, from_code);
        state.locked = init.store().get_u8(KEY_LOCKED) == Some(1);
        init.set_rotation(state.rotation);
    }

    /// Takes the IMU's pose callback: an app that also wants poses calls
    /// `Input::set_rotation` from its own instead of using this type.
    fn start(&mut self, context: &PluginContext) {
        self.0.state.lock().context = Some((context.store().clone(), context.input().clone()));
        if let Some(imu) = self.0.imu {
            let inner = self.0.clone();
            imu.set_orientation_cb(Some(Box::new(move |pose| inner.pose(pose))));
            imu.set_orientation_enabled(!self.locked());
        }
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
        if let Some((store, input)) = &state.context {
            input.set_rotation(rotation);
            store.set_u8(KEY_ROTATION, code(rotation));
        }
    }
}
