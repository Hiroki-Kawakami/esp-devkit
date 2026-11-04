/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::time::Duration;

use bsp_rs::display::Rotation;
use idf::log::error;
use idf::nvs::{Key, Nvs};
use idf::sync::{Mutex, OnceLock};
use idf::freertos::Timer;

use crate::app::Input;
use crate::theme::ColorMode;

const TAG: &CStr = c"store";
const SAVE_DELAY: Duration = Duration::from_secs(5);
const KEY_COLOR_MODE: Key<u8> = Key::new(c"color_mode");

/// What plugins hear about, on the UI task.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiEvent {
    Rotated(Rotation),
    ColorMode(ColorMode),
}

/// Adds to what an app does, without it knowing the type. Listed in
/// `AppConfig::plugins`; a type that has more to offer stays with the app as a
/// clone of a shared handle.
pub trait Plugin {
    /// Before anything is drawn, to set how the UI starts.
    fn init(&mut self, _init: &mut PluginInit<'_>) {}

    /// Once the app exists, to register callbacks.
    fn start(&mut self, _context: &PluginContext) {}

    /// Blocks the UI, so what takes long belongs on a task or a timer.
    fn on_event(&mut self, _event: UiEvent, _context: &PluginContext) {}
}

pub struct PluginInit<'a> {
    pub(crate) store: &'a Store,
    pub(crate) rotation: Rotation,
    pub(crate) color_mode: ColorMode,
}

impl PluginInit<'_> {
    pub fn store(&self) -> &Store {
        self.store
    }

    pub fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    pub fn set_color_mode(&mut self, mode: ColorMode) {
        self.color_mode = mode;
    }
}

pub struct PluginContext {
    pub(crate) store: Store,
    pub(crate) input: Input,
}

impl PluginContext {
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn input(&self) -> &Input {
        &self.input
    }
}

#[derive(Default)]
struct Keys {
    nvs: Option<Nvs>,
    pending: Vec<(Key<u8>, u8)>,
}

struct Inner {
    namespace: &'static CStr,
    keys: Mutex<Keys>,
    timer: OnceLock<Timer>,
}

/// Small settings in one NVS namespace, opened on first use. Writes wait for a
/// quiet moment, as flash stops both cores, and are lost if power goes first.
#[derive(Clone)]
pub struct Store(Arc<Inner>);

impl Store {
    pub(crate) fn new(namespace: &'static CStr) -> Self {
        Self(Arc::new(Inner { namespace, keys: Mutex::new(Keys::default()), timer: OnceLock::new() }))
    }

    /// `None` when the key was never written.
    pub fn get_u8(&self, key: Key<u8>) -> Option<u8> {
        let mut keys = self.0.keys.lock();
        if let Some(&(_, value)) = keys.pending.iter().find(|(k, _)| *k == key) {
            return Some(value);
        }
        self.0.open(&mut keys).and_then(|nvs| nvs.get(key).inspect_err(|err| error!(TAG, "get: {err}")).ok().flatten())
    }

    pub fn set_u8(&self, key: Key<u8>, value: u8) {
        let mut keys = self.0.keys.lock();
        match keys.pending.iter_mut().find(|(k, _)| *k == key) {
            Some(pending) => pending.1 = value,
            None => keys.pending.push((key, value)),
        }
        drop(keys);
        if self.0.timer.get().is_none() {
            let inner = self.0.clone();
            match Timer::one_shot(c"store", SAVE_DELAY, move || inner.save()) {
                Ok(timer) => {
                    let _ = self.0.timer.set(timer);
                }
                Err(err) => error!(TAG, "timer: {err}"),
            }
        }
        if let Some(Err(err)) = self.0.timer.get().map(Timer::reset) {
            error!(TAG, "timer: {err}");
        }
    }
}

impl Inner {
    fn open<'a>(&self, keys: &'a mut Keys) -> Option<&'a mut Nvs> {
        if keys.nvs.is_none() {
            let opened = Nvs::open(self.namespace);
            keys.nvs = opened.inspect_err(|err| error!(TAG, "open: {err}")).ok();
        }
        keys.nvs.as_mut()
    }

    fn save(&self) {
        let mut keys = self.keys.lock();
        let pending = core::mem::take(&mut keys.pending);
        let Some(nvs) = self.open(&mut keys) else { return };
        let saved = pending.iter().try_for_each(|&(key, value)| nvs.set(key, value)).and_then(|()| nvs.commit());
        if let Err(err) = saved {
            error!(TAG, "save: {err}");
        }
    }
}

/// Keeps the color mode across restarts, whatever sets it.
#[derive(Clone, Copy, Default)]
pub struct SavedColorMode;

impl SavedColorMode {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for SavedColorMode {
    fn init(&mut self, init: &mut PluginInit<'_>) {
        if init.store().get_u8(KEY_COLOR_MODE) == Some(1) {
            init.set_color_mode(ColorMode::Dark);
        }
    }

    fn on_event(&mut self, event: UiEvent, context: &PluginContext) {
        if let UiEvent::ColorMode(mode) = event {
            context.store().set_u8(KEY_COLOR_MODE, u8::from(mode == ColorMode::Dark));
        }
    }
}
