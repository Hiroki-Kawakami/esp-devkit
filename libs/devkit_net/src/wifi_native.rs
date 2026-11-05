/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The chip's own Wi-Fi station.
 */

use alloc::vec::Vec;
use core::ffi::{CStr, c_void};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use idf::log::{info, warn};
use idf::sync::{Mutex, OnceLock};
use idf::time::Instant;
use idf::{Error, Result, thread};

use crate::link::{Link, LinkInfo, LinkRx, Medium, RxFrame, TxError};
use crate::wifi::{
    Auth, BoxFuture, DisconnectReason, PowerSave, ScanRecord, Security, StaConfig, WifiDriver, WifiEvents, WifiState,
    offload,
};

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unnecessary_transmutes,
    unsafe_op_in_unsafe_fn
)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"devkit_net_wifi";

const MTU: usize = 1500;
const START_WAIT: Duration = Duration::from_secs(5);
const SCAN_MAX: usize = 32;

struct Sta;
struct Native;

struct Radio {
    initialized: bool,
    started: bool,
}

static STA: Sta = Sta;
static NATIVE: Native = Native;
static LINK_RX: OnceLock<LinkRx> = OnceLock::new();
static EVENTS: OnceLock<WifiEvents> = OnceLock::new();
static RADIO: Mutex<Radio> = Mutex::new(Radio { initialized: false, started: false });
static POWER_SAVE: Mutex<PowerSave> = Mutex::new(PowerSave::Min);
static STARTED: AtomicBool = AtomicBool::new(false);
static UP: AtomicBool = AtomicBool::new(false);

pub(crate) fn register() {
    crate::wifi::register_wifi(&NATIVE);
}

fn sta_mac() -> [u8; 6] {
    let mut mac = [0; 6];
    if unsafe { ffi::esp_read_mac(mac.as_mut_ptr(), ffi::esp_mac_type_t_ESP_MAC_WIFI_STA) } != 0 {
        warn!(TAG, "no station mac");
    }
    mac
}

impl Link for Sta {
    fn info(&self) -> LinkInfo {
        LinkInfo { medium: Medium::Ethernet, mac: sta_mac(), mtu: MTU }
    }

    fn attach(&self, rx: LinkRx) {
        let _ = LINK_RX.set(rx);
    }

    #[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
    fn transmit(&self, frame: &[u8]) -> core::result::Result<(), TxError> {
        if !UP.load(Ordering::Acquire) {
            return Err(TxError::Down);
        }
        let len = u16::try_from(frame.len()).map_err(|_| TxError::Busy)?;
        let sent = unsafe { ffi::esp_wifi_internal_tx(ffi::wifi_interface_t_WIFI_IF_STA, frame.as_ptr().cast_mut().cast(), len) };
        if sent == 0 { Ok(()) } else { Err(TxError::Busy) }
    }
}

#[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
unsafe fn release(_: NonNull<u8>, _: usize, eb: usize) {
    unsafe { ffi::esp_wifi_internal_free_rx_buffer(eb as *mut c_void) };
}

/// On the Wi-Fi task. The frame stays in the driver's buffer until the stack
/// has processed it.
#[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
unsafe extern "C" fn on_rx(buffer: *mut c_void, len: u16, eb: *mut c_void) -> ffi::esp_err_t {
    let (Some(rx), Some(data)) = (LINK_RX.get(), NonNull::new(buffer.cast::<u8>())) else {
        unsafe { ffi::esp_wifi_internal_free_rx_buffer(eb) };
        return 0;
    };
    rx.deliver(unsafe { RxFrame::from_raw(data, usize::from(len), eb as usize, release) });
    0
}

/// On the Wi-Fi task, for every frame sent.
#[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
unsafe extern "C" fn on_tx_done(_: u8, _: *mut u8, _: *mut u16, _: bool) {
    if let Some(rx) = LINK_RX.get() {
        rx.tx_ready();
    }
}

/// On the default event loop's task.
unsafe extern "C" fn on_event(_: *mut c_void, base: ffi::esp_event_base_t, id: i32, data: *mut c_void) {
    if base != unsafe { ffi::WIFI_EVENT } {
        return;
    }
    match id as u32 {
        ffi::wifi_event_t_WIFI_EVENT_STA_START => STARTED.store(true, Ordering::Release),
        ffi::wifi_event_t_WIFI_EVENT_STA_STOP => STARTED.store(false, Ordering::Release),
        ffi::wifi_event_t_WIFI_EVENT_STA_CONNECTED => {
            let e = unsafe { &*data.cast::<ffi::wifi_event_sta_connected_t>() };
            if let Some(events) = EVENTS.get() {
                events.set_state(WifiState::Connected { bssid: e.bssid, channel: e.channel });
            }
            UP.store(true, Ordering::Release);
            if let Some(rx) = LINK_RX.get() {
                rx.set_up(true);
            }
        }
        ffi::wifi_event_t_WIFI_EVENT_STA_DISCONNECTED => {
            let e = unsafe { &*data.cast::<ffi::wifi_event_sta_disconnected_t>() };
            UP.store(false, Ordering::Release);
            if let Some(rx) = LINK_RX.get() {
                rx.set_up(false);
            }
            if let Some(events) = EVENTS.get() {
                events.set_state(WifiState::Disconnected(Some(DisconnectReason::from_code(e.reason.into()))));
            }
        }
        _ => {}
    }
}

fn check(code: ffi::esp_err_t) -> Result<()> {
    Error::check(code)
}

fn apply_power_save(mode: PowerSave) -> Result<()> {
    let ps = match mode {
        PowerSave::None => ffi::wifi_ps_type_t_WIFI_PS_NONE,
        PowerSave::Min => ffi::wifi_ps_type_t_WIFI_PS_MIN_MODEM,
        PowerSave::Max => ffi::wifi_ps_type_t_WIFI_PS_MAX_MODEM,
    };
    check(unsafe { ffi::esp_wifi_set_ps(ps) })
}

fn init_radio() -> Result<()> {
    let loop_err = unsafe { ffi::esp_event_loop_create_default() };
    if loop_err != 0 && loop_err != Error::INVALID_STATE.code() {
        check(loop_err)?;
    }
    check(unsafe {
        ffi::esp_event_handler_register(ffi::WIFI_EVENT, ffi::ESP_EVENT_ANY_ID, Some(on_event), ptr::null_mut())
    })?;
    if let Err(err) = idf::nvs::init() {
        warn!(TAG, "nvs: {err}; RF calibration runs in full");
    }
    let mut init: ffi::wifi_init_config_t = unsafe { core::mem::zeroed() };
    unsafe { ffi::devkit_net_wifi_init_default(&mut init) };
    init.nvs_enable = 0;
    check(unsafe { ffi::esp_wifi_init(&init) })?;
    check(unsafe { ffi::esp_wifi_set_storage(ffi::wifi_storage_t_WIFI_STORAGE_RAM) })?;
    check(unsafe { ffi::esp_wifi_set_mode(ffi::wifi_mode_t_WIFI_MODE_STA) })?;
    check(unsafe { ffi::esp_wifi_internal_reg_rxcb(ffi::wifi_interface_t_WIFI_IF_STA, Some(on_rx)) })
}

fn wait_started(started: bool) -> Result<()> {
    let deadline = Instant::now() + START_WAIT;
    while STARTED.load(Ordering::Acquire) != started {
        if Instant::now() >= deadline {
            return Err(Error::TIMEOUT);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn start_radio() -> Result<()> {
    let mut radio = RADIO.lock();
    if radio.started {
        return Ok(());
    }
    if !radio.initialized {
        init_radio()?;
        radio.initialized = true;
    }
    check(unsafe { ffi::esp_wifi_start() })?;
    check(unsafe { ffi::esp_wifi_set_tx_done_cb(Some(on_tx_done)) })?;
    apply_power_save(*POWER_SAVE.lock())?;
    wait_started(true)?;
    radio.started = true;
    info!(TAG, "station up");
    Ok(())
}

fn stop_radio() -> Result<()> {
    let mut radio = RADIO.lock();
    if !radio.started {
        return Ok(());
    }
    check(unsafe { ffi::esp_wifi_stop() })?;
    radio.started = false;
    UP.store(false, Ordering::Release);
    if let Some(rx) = LINK_RX.get() {
        rx.set_up(false);
    }
    wait_started(false)?;
    info!(TAG, "station stopped");
    Ok(())
}

fn scan() -> Result<Vec<ScanRecord>> {
    let mut records: Vec<ffi::devkit_net_scan_record_t> = Vec::with_capacity(SCAN_MAX);
    let mut count = SCAN_MAX as u16;
    check(unsafe { ffi::devkit_net_wifi_scan(records.as_mut_ptr(), &mut count) })?;
    unsafe { records.set_len(usize::from(count).min(SCAN_MAX)) };
    Ok(records
        .iter()
        .map(|r| ScanRecord {
            ssid: r.ssid[..usize::from(r.ssid_len)].to_vec(),
            bssid: r.bssid,
            channel: r.channel,
            rssi: r.rssi,
            security: Security::from_auth_mode(r.authmode.into()),
        })
        .collect())
}

struct Owned {
    ssid: Vec<u8>,
    password: Option<Vec<u8>>,
    bssid: Option<[u8; 6]>,
    channel: Option<u8>,
}

fn join(config: &Owned) -> Result<()> {
    let password = config.password.as_ref().map(|p| {
        let mut p = p.clone();
        p.push(0);
        p
    });
    check(unsafe {
        ffi::devkit_net_wifi_set_sta(
            config.ssid.as_ptr(),
            config.ssid.len(),
            password.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
            config.bssid.as_ref().map_or(ptr::null(), |b| b.as_ptr()),
            config.channel.unwrap_or(0),
        )
    })?;
    check(unsafe { ffi::esp_wifi_connect() })
}

impl WifiDriver for Native {
    fn sta(&self) -> &'static dyn Link {
        &STA
    }

    fn attach(&self, events: WifiEvents) {
        let _ = EVENTS.set(events);
    }

    fn start(&self) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("wifi_start", start_radio)
    }

    fn stop(&self) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("wifi_stop", stop_radio)
    }

    fn scan(&self) -> BoxFuture<'_, core::result::Result<Vec<ScanRecord>, i32>> {
        offload("wifi_scan", scan)
    }

    fn rssi(&self) -> BoxFuture<'_, core::result::Result<i8, i32>> {
        offload("wifi_rssi", || {
            let mut rssi = 0;
            check(unsafe { ffi::esp_wifi_sta_get_rssi(&mut rssi) })?;
            Ok(rssi as i8)
        })
    }

    fn connect<'a>(&'a self, config: &'a StaConfig<'a>) -> BoxFuture<'a, core::result::Result<(), i32>> {
        let owned = Owned {
            ssid: config.ssid.to_vec(),
            password: match config.auth {
                Auth::Open => None,
                Auth::Psk(p) => Some(p.as_bytes().to_vec()),
            },
            bssid: config.bssid,
            channel: config.channel,
        };
        offload("wifi_join", move || join(&owned))
    }

    fn set_power_save(&self, mode: PowerSave) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("wifi_ps", move || {
            *POWER_SAVE.lock() = mode;
            if !RADIO.lock().started {
                return Ok(());
            }
            apply_power_save(mode)
        })
    }

    fn disconnect(&self) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("wifi_leave", || {
            if !RADIO.lock().started {
                return Ok(());
            }
            check(unsafe { ffi::esp_wifi_disconnect() })
        })
    }
}
