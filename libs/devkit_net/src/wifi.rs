/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use idf::executor::{self, CRITICAL, Signal};
use idf::log::error;
use idf::sync::OnceLock;
use idf::{Error, thread};

use crate::iface::{Interface, add_link};
use crate::link::Link;
use crate::net::Ipv4Mode;
use crate::watch::Watch;

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
    /// The radio is not started.
    Stopped,
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
    /// The radio is not started.
    Stopped,
    NotFound,
    AuthFailed,
    /// Disconnected while joining, for another reason.
    Rejected(DisconnectReason),
    /// The driver's own error code.
    Driver(i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    Open,
    Wep,
    WpaPsk,
    /// WPA2, or WPA and WPA2.
    Wpa2Psk,
    /// WPA3, or WPA2 and WPA3.
    Wpa3Sae,
    Enterprise,
    /// WAPI, OWE, DPP or what the radio could not parse.
    Other,
}

impl Security {
    /// From an ESP-IDF `wifi_auth_mode_t` value.
    pub fn from_auth_mode(mode: u32) -> Self {
        match mode {
            0 => Self::Open,
            1 => Self::Wep,
            2 => Self::WpaPsk,
            3 | 4 => Self::Wpa2Psk,
            6 | 7 => Self::Wpa3Sae,
            5 | 10 | 14 | 15 | 16 => Self::Enterprise,
            _ => Self::Other,
        }
    }

    /// Whether `connect` can join a network that uses it.
    pub fn is_supported(self) -> bool {
        matches!(self, Self::Open | Self::Wpa2Psk | Self::Wpa3Sae)
    }
}

/// One access point a scan saw.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScanRecord {
    pub ssid: Vec<u8>,
    pub bssid: [u8; 6],
    pub channel: u8,
    pub rssi: i8,
    pub security: Security,
}

/// A Wi-Fi radio, implemented by whoever owns it.
pub trait WifiDriver: Send + Sync {
    /// The station interface's link.
    fn sta(&self) -> &'static dyn Link;
    /// Called once at registration; state changes go to `events` from then
    /// on, and the station link goes up and down with the connection.
    fn attach(&self, events: WifiEvents);
    /// Returns once the station runs; a started radio is not an error.
    fn start(&self) -> BoxFuture<'_, Result<(), i32>>;
    /// Leaves the network and stops the radio; a stopped radio is not an error.
    fn stop(&self) -> BoxFuture<'_, Result<(), i32>>;
    /// Asks to join; only called while started. The outcome arrives through
    /// `events`.
    fn connect<'a>(&'a self, config: &'a StaConfig<'a>) -> BoxFuture<'a, Result<(), i32>>;
    fn disconnect(&self) -> BoxFuture<'_, Result<(), i32>>;
    /// Returns once the scan is done; only called while started. Hidden
    /// networks are left out.
    fn scan(&self) -> BoxFuture<'_, Result<Vec<ScanRecord>, i32>>;
    /// Of the joined access point; only called while connected.
    fn rssi(&self) -> BoxFuture<'_, Result<i8, i32>>;
    /// Kept for a radio that is not started yet.
    fn set_power_save(&self, mode: PowerSave) -> BoxFuture<'_, Result<(), i32>>;
}

type Shared = Watch<WifiState>;

/// Where a driver reports its state.
#[derive(Clone)]
pub struct WifiEvents(&'static Shared);

impl WifiEvents {
    /// Ignored while the radio is stopped.
    pub fn set_state(&self, state: WifiState) {
        self.0.update(|&now| (now != WifiState::Stopped && state != WifiState::Stopped).then_some(state));
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
        let shared: &'static Shared = Box::leak(Box::new(Watch::new(WifiState::Stopped)));
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
pub(crate) fn offload<T: Send + 'static>(
    name: &'static str,
    f: impl FnOnce() -> idf::Result<T> + Send + 'static,
) -> BoxFuture<'static, Result<T, i32>> {
    let done = Arc::new(Signal::new(&CRITICAL));
    let tx = done.clone();
    let spawned = thread::Builder::new().name(name).stack_size(CONTROL_STACK).spawn(move || {
        let result = f();
        if let Err(err) = &result {
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
    pub(crate) fn watch(&self) -> &'static Watch<WifiState> {
        self.shared
    }

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

    /// Returns once the radio runs. A started radio is not an error.
    pub async fn start(&self) -> Result<(), ConnectError> {
        if self.state() != WifiState::Stopped {
            return Ok(());
        }
        self.driver.start().await.map_err(ConnectError::Driver)?;
        if self.state() == WifiState::Stopped {
            self.shared.set(WifiState::Disconnected(None));
        }
        Ok(())
    }

    /// Leaves the network and stops the radio.
    pub async fn stop(&self) -> Result<(), ConnectError> {
        if self.state() == WifiState::Stopped {
            return Ok(());
        }
        self.driver.stop().await.map_err(ConnectError::Driver)?;
        self.shared.set(WifiState::Stopped);
        Ok(())
    }

    /// The access points in range; the radio must be started.
    pub async fn scan(&self) -> Result<Vec<ScanRecord>, ConnectError> {
        if self.state() == WifiState::Stopped {
            return Err(ConnectError::Stopped);
        }
        self.driver.scan().await.map_err(ConnectError::Driver)
    }

    /// Of the joined access point, in dBm. `None` while not connected.
    pub async fn rssi(&self) -> Option<i8> {
        if !matches!(self.state(), WifiState::Connected { .. }) {
            return None;
        }
        self.driver.rssi().await.ok()
    }

    /// Returns once joined; leaves the current network first. The radio must
    /// be started.
    pub async fn connect(&self, config: &StaConfig<'_>) -> Result<(), ConnectError> {
        match self.state() {
            WifiState::Stopped => return Err(ConnectError::Stopped),
            WifiState::Disconnected(_) => {}
            _ => self.disconnect().await?,
        }
        let mut seen = self.shared.set(WifiState::Connecting);
        self.driver.connect(config).await.map_err(ConnectError::Driver)?;
        loop {
            let (state, version) = self.shared.after(seen).await;
            seen = version;
            match state {
                WifiState::Connected { .. } => return Ok(()),
                WifiState::Connecting => {}
                WifiState::Stopped => return Err(ConnectError::Stopped),
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
        let (state, seen) = self.shared.get();
        if state == WifiState::Stopped {
            return Ok(());
        }
        self.driver.disconnect().await.map_err(ConnectError::Driver)?;
        let _ = executor::timeout(DISCONNECT_WAIT, async {
            let mut seen = seen;
            loop {
                let (state, version) = self.shared.after(seen).await;
                seen = version;
                if matches!(state, WifiState::Disconnected(_) | WifiState::Stopped) {
                    break;
                }
            }
        })
        .await;
        if self.state() != WifiState::Stopped {
            self.shared.set(WifiState::Disconnected(Some(DisconnectReason::Requested)));
        }
        Ok(())
    }
}
