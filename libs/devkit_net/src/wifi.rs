/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::Pin;
use core::task::{Poll, Waker};
use core::time::Duration;

use idf::executor::{self, CRITICAL, Signal};
use idf::log::error;
use idf::sync::{Mutex, OnceLock};
use idf::{Error, thread};

use crate::iface::{Interface, add_link};
use crate::link::Link;
use crate::net::Ipv4Mode;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

const DISCONNECT_WAIT: Duration = Duration::from_secs(2);
const CONTROL_STACK: usize = 6144;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Auth<'a> {
    Open,
    /// WPA2 or WPA3 personal, whichever the AP offers.
    Psk(&'a str),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StaConfig<'a> {
    pub ssid: &'a [u8],
    pub auth: Auth<'a>,
    pub bssid: Option<[u8; 6]>,
    pub channel: Option<u8>,
}

impl<'a> StaConfig<'a> {
    /// Open when `password` is empty.
    pub fn new(ssid: &'a str, password: &'a str) -> Self {
        let auth = if password.is_empty() { Auth::Open } else { Auth::Psk(password) };
        Self { ssid: ssid.as_bytes(), auth, bssid: None, channel: None }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DisconnectReason {
    NotFound,
    AuthFailed,
    Lost,
    Requested,
    Other(u16),
}

impl DisconnectReason {
    /// From an 802.11 or ESP-IDF `wifi_err_reason_t` code.
    pub fn from_code(code: u16) -> Self {
        match code {
            201 | 210 | 211 | 212 => Self::NotFound,
            15 | 202 | 204 => Self::AuthFailed,
            4 | 200 => Self::Lost,
            8 => Self::Requested,
            other => Self::Other(other),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WifiState {
    Disconnected(Option<DisconnectReason>),
    Connecting,
    Connected { bssid: [u8; 6], channel: u8 },
}

/// Radio power saving while connected.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PowerSave {
    /// Always listening: the lowest latency.
    None,
    /// Wakes for every DTIM beacon.
    #[default]
    Min,
    /// Wakes at the listen interval only.
    Max,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectError {
    NotFound,
    AuthFailed,
    /// Disconnected while joining, for another reason.
    Rejected(DisconnectReason),
    /// The driver's own error code.
    Driver(i32),
}

/// A Wi-Fi radio, implemented by whoever owns it.
pub trait WifiDriver: Send + Sync {
    /// The station interface's link.
    fn sta(&self) -> &'static dyn Link;
    /// Called once at registration; state changes go to `events` from then
    /// on, and the station link goes up and down with the connection.
    fn attach(&self, events: WifiEvents);
    /// Starts the radio if needed and asks to join. The outcome arrives
    /// through `events`.
    fn connect<'a>(&'a self, config: &'a StaConfig<'a>) -> BoxFuture<'a, Result<(), i32>>;
    fn disconnect(&self) -> BoxFuture<'_, Result<(), i32>>;
    /// Kept for a radio that is not up yet.
    fn set_power_save(&self, mode: PowerSave) -> BoxFuture<'_, Result<(), i32>>;
}

struct State {
    state: WifiState,
    version: u32,
    waiters: Vec<Waker>,
}

struct Shared {
    state: Mutex<State>,
}

impl Shared {
    fn set(&self, state: WifiState) -> u32 {
        let mut s = self.state.lock();
        s.state = state;
        s.version = s.version.wrapping_add(1);
        for w in s.waiters.drain(..) {
            w.wake();
        }
        s.version
    }

    fn get(&self) -> (WifiState, u32) {
        let s = self.state.lock();
        (s.state, s.version)
    }

    /// The state once its version differs from `seen`.
    async fn after(&self, seen: u32) -> (WifiState, u32) {
        poll_fn(|cx| {
            let mut s = self.state.lock();
            if s.version != seen {
                return Poll::Ready((s.state, s.version));
            }
            if !s.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                s.waiters.push(cx.waker().clone());
            }
            Poll::Pending
        })
        .await
    }
}

/// Where a driver reports its state.
#[derive(Clone)]
pub struct WifiEvents(&'static Shared);

impl WifiEvents {
    pub fn set_state(&self, state: WifiState) {
        self.0.set(state);
    }
}

pub struct Wifi {
    driver: &'static dyn WifiDriver,
    shared: &'static Shared,
    sta: Interface,
}

static WIFI: OnceLock<Wifi> = OnceLock::new();

/// Makes `driver` the radio `wifi()` returns; one per firmware. Its station
/// interface is named "sta" and starts with `Ipv4Mode::Dhcp`.
pub fn register_wifi(driver: &'static dyn WifiDriver) -> Option<Interface> {
    let mut registered = None;
    WIFI.get_or_init(|| {
        let shared: &'static Shared = Box::leak(Box::new(Shared {
            state: Mutex::new(State { state: WifiState::Disconnected(None), version: 0, waiters: Vec::new() }),
        }));
        let sta = add_link("sta", driver.sta());
        sta.set_ipv4(Ipv4Mode::Dhcp);
        driver.attach(WifiEvents(shared));
        registered = Some(sta);
        Wifi { driver, shared, sta }
    });
    registered
}

pub fn wifi() -> Option<&'static Wifi> {
    crate::net::registry();
    WIFI.get()
}

/// Runs the blocking calls of `f` on a task of their own, so the caller's
/// executor goes on meanwhile.
#[cfg_attr(not(CONFIG_DEVKIT_NET_WIFI), allow(dead_code))]
pub(crate) fn offload(
    name: &'static str,
    f: impl FnOnce() -> idf::Result<()> + Send + 'static,
) -> BoxFuture<'static, Result<(), i32>> {
    let done = Arc::new(Signal::new(&CRITICAL));
    let tx = done.clone();
    let spawned = thread::Builder::new().name(name).stack_size(CONTROL_STACK).spawn(move || {
        let result = f();
        if let Err(err) = result {
            error!(c"devkit_net_wifi", "{name}: {err}");
        }
        tx.signal(result.map_err(Error::code));
    });
    Box::pin(async move {
        match spawned {
            Ok(_) => done.wait().await,
            Err(err) => Err(err.code()),
        }
    })
}

impl Wifi {
    pub fn sta(&self) -> Interface {
        self.sta
    }

    pub fn state(&self) -> WifiState {
        self.shared.get().0
    }

    /// The next state after this call.
    pub async fn changed(&self) -> WifiState {
        let (_, seen) = self.shared.get();
        self.shared.after(seen).await.0
    }

    /// Returns once joined; leaves the current network first.
    pub async fn connect(&self, config: &StaConfig<'_>) -> Result<(), ConnectError> {
        if !matches!(self.state(), WifiState::Disconnected(_)) {
            self.disconnect().await?;
        }
        let mut seen = self.shared.set(WifiState::Connecting);
        self.driver.connect(config).await.map_err(ConnectError::Driver)?;
        loop {
            let (state, version) = self.shared.after(seen).await;
            seen = version;
            match state {
                WifiState::Connected { .. } => return Ok(()),
                WifiState::Connecting => {}
                WifiState::Disconnected(reason) => {
                    return Err(match reason {
                        Some(DisconnectReason::NotFound) => ConnectError::NotFound,
                        Some(DisconnectReason::AuthFailed) => ConnectError::AuthFailed,
                        other => ConnectError::Rejected(other.unwrap_or(DisconnectReason::Other(0))),
                    });
                }
            }
        }
    }

    pub async fn set_power_save(&self, mode: PowerSave) -> Result<(), ConnectError> {
        self.driver.set_power_save(mode).await.map_err(ConnectError::Driver)
    }

    pub async fn disconnect(&self) -> Result<(), ConnectError> {
        let (_, seen) = self.shared.get();
        self.driver.disconnect().await.map_err(ConnectError::Driver)?;
        let _ = executor::timeout(DISCONNECT_WAIT, async {
            let mut seen = seen;
            loop {
                let (state, version) = self.shared.after(seen).await;
                seen = version;
                if matches!(state, WifiState::Disconnected(_)) {
                    break;
                }
            }
        })
        .await;
        self.shared.set(WifiState::Disconnected(Some(DisconnectReason::Requested)));
        Ok(())
    }
}
