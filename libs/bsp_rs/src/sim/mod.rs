/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

// Shared with tools/devkit, which uses the parts this side does not.
#[allow(dead_code)]
pub(crate) mod protocol;
pub(crate) mod audio;
pub(crate) mod imu;
pub(crate) mod panel;
pub(crate) mod touch;

use alloc::boxed::Box;
use core::ffi::{CStr, c_int, c_void};
use core::ptr;

use idf::log::{error, warn};
use idf::sync::{Mutex, OnceLock};
use idf::thread::Builder;

use self::protocol::{ENV_SOCKET_FD, EXIT_RESTART, Event, RECORD_SIZE, TouchPoint, VERSION};

mod ffi {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"sim";

pub(crate) type TouchListener = Box<dyn Fn(&[TouchPoint]) + Send + Sync>;
pub(crate) type AudioListener = Box<dyn Fn(u32, u64) + Send + Sync>;
pub(crate) type ImuListener = Box<dyn Fn([f32; 3], [f32; 3]) + Send + Sync>;

pub(crate) struct Host {
    fd: c_int,
    send_lock: Mutex<()>,
    psram_base: usize,
    psram_size: usize,
}

static HOST: OnceLock<Option<Host>> = OnceLock::new();
static TOUCH: OnceLock<TouchListener> = OnceLock::new();
static AUDIO: OnceLock<AudioListener> = OnceLock::new();
static IMU: OnceLock<ImuListener> = OnceLock::new();

/// `None` when the firmware does not run under the simulator host.
pub(crate) fn host() -> Option<&'static Host> {
    HOST.get_or_init(connect).as_ref()
}

fn connect() -> Option<Host> {
    let value = unsafe { ffi::getenv(ENV_SOCKET_FD.as_ptr()) };
    if value.is_null() {
        return None;
    }
    let fd: c_int = unsafe { CStr::from_ptr(value) }.to_str().ok()?.parse().ok()?;
    let (mut base, mut size): (*mut c_void, usize) = (ptr::null_mut(), 0);
    if !unsafe { ffi::sim_psram_region(&mut base, &mut size) } {
        error!(TAG, "no PSRAM region from the host");
        return None;
    }
    if let Err(err) = Builder::new().name("sim_link").stack_size(4096).spawn(move || receive(fd)) {
        error!(TAG, "receiver task: {err}");
        return None;
    }
    let host = Host { fd, send_lock: Mutex::try_new(()).ok()?, psram_base: base as usize, psram_size: size };
    host.send(&Event::Hello { version: VERSION });
    Some(host)
}

impl Host {
    pub(crate) fn send(&self, event: &Event) {
        let record = event.encode();
        let _guard = self.send_lock.lock();
        let mut at = 0;
        while at < RECORD_SIZE {
            let n = unsafe { ffi::send(self.fd, record[at..].as_ptr().cast(), RECORD_SIZE - at, 0) };
            if n <= 0 {
                warn!(TAG, "send to host failed");
                return;
            }
            at += n as usize;
        }
    }

    /// Offset of `p` in the shared PSRAM region.
    pub(crate) fn psram_offset(&self, p: *const u8) -> Option<u64> {
        let offset = (p as usize).checked_sub(self.psram_base)?;
        (offset < self.psram_size).then_some(offset as u64)
    }

    /// Runs on the receiver task. Only the first listener is kept.
    pub(crate) fn set_touch_listener(&self, listener: TouchListener) {
        if TOUCH.set(listener).is_err() {
            warn!(TAG, "touch listener already set");
        }
    }

    /// Runs on the receiver task with a stream and the bytes it has played.
    /// Only the first listener is kept.
    pub(crate) fn set_audio_listener(&self, listener: AudioListener) {
        if AUDIO.set(listener).is_err() {
            warn!(TAG, "audio listener already set");
        }
    }

    /// Runs on the receiver task. Only the first listener is kept.
    pub(crate) fn set_imu_listener(&self, listener: ImuListener) {
        if IMU.set(listener).is_err() {
            warn!(TAG, "imu listener already set");
        }
    }
}

// The host closing its end means it went away, so the firmware goes too.
fn receive(fd: c_int) {
    let mut record = [0u8; RECORD_SIZE];
    loop {
        let mut at = 0;
        while at < RECORD_SIZE {
            let n = unsafe { ffi::recv(fd, record[at..].as_mut_ptr().cast(), RECORD_SIZE - at, 0) };
            if n <= 0 {
                unsafe { ffi::exit(0) };
            }
            at += n as usize;
        }
        match Event::decode(&record) {
            Some(Event::Touch { count, points }) => {
                if let Some(listener) = TOUCH.get() {
                    listener(&points[..usize::from(count)]);
                }
            }
            Some(Event::AudioPlayed { stream, bytes }) => {
                if let Some(listener) = AUDIO.get() {
                    listener(stream, bytes);
                }
            }
            Some(Event::Imu { accel, gyro }) => {
                if let Some(listener) = IMU.get() {
                    listener(accel, gyro);
                }
            }
            Some(_) => {}
            None => warn!(TAG, "unknown event 0x{:02x}", record[0]),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sim_restart_hook() {
    if host().is_some() {
        unsafe { ffi::exit(EXIT_RESTART) };
    }
}
