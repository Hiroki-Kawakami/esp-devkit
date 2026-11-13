/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The coprocessor's station as a devkit_net radio.
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use devkit_net::{
    Auth, BoxFuture, DisconnectReason, Link, LinkInfo, LinkRx, Medium, PowerSave, RxFrame, ScanRecord, Security,
    StaConfig, TxError, WifiDriver, WifiEvents, WifiState,
};
use idf::executor::{CRITICAL, Signal};
use idf::log::{error, info};
use idf::sync::{Mutex, OnceLock};
use idf::time::Instant;
use idf::{Error, Result, sys, thread};

use super::ffi;
use super::transport::RxBuf;
use super::wire::If;
use super::wifi;

const TAG: &CStr = c"esp_hosted_net";

const MTU: usize = 1500;
const TX_WAIT: Duration = Duration::from_millis(20);
const START_WAIT: Duration = Duration::from_secs(5);
const CONTROL_STACK: usize = 6144;
const SCAN_MAX: u16 = 32;

struct Sta;

struct Radio {
    initialized: bool,
    started: bool,
    mac: [u8; 6],
    power_save: PowerSave,
}

struct Hosted;

static STA: Sta = Sta;
static HOSTED: Hosted = Hosted;
static LINK_RX: OnceLock<LinkRx> = OnceLock::new();
static EVENTS: OnceLock<WifiEvents> = OnceLock::new();
static RADIO: Mutex<Radio> = Mutex::new(Radio { initialized: false, started: false, mac: [0; 6], power_save: PowerSave::Min });
static UP: AtomicBool = AtomicBool::new(false);

/// Makes the coprocessor the firmware's Wi-Fi radio. Nothing reaches the
/// coprocessor until the radio starts.
pub(crate) fn register() {
    devkit_net::register_wifi(&HOSTED);
}

fn check(code: sys::esp_err_t) -> Result<()> {
    Error::check(code)
}

fn apply_power_save(mode: PowerSave) -> Result<()> {
    let ps = match mode {
        PowerSave::None => ffi::wifi_ps_type_t_WIFI_PS_NONE,
        PowerSave::Min => ffi::wifi_ps_type_t_WIFI_PS_MIN_MODEM,
        PowerSave::Max => ffi::wifi_ps_type_t_WIFI_PS_MAX_MODEM,
    };
    check(unsafe { wifi::esp_wifi_remote_set_ps(ps) })
}

fn link_info() -> LinkInfo {
    LinkInfo { medium: Medium::Ethernet, mac: RADIO.lock().mac, mtu: MTU }
}

impl Link for Sta {
    fn info(&self) -> LinkInfo {
        link_info()
    }

    fn attach(&self, rx: LinkRx) {
        let _ = LINK_RX.set(rx);
    }

    fn transmit(&self, frame: &[u8]) -> core::result::Result<(), TxError> {
        if !UP.load(Ordering::Acquire) {
            return Err(TxError::Down);
        }
        let hosted = super::linked().map_err(|_| TxError::Down)?;
        hosted.transport.send(If::Sta, 0, frame, Some(TX_WAIT)).map_err(|_| TxError::Busy)
    }
}

/// On the transport rx task; `false` when no stack took the frame.
pub(super) fn on_sta(frame: &[u8], held: Option<&'static RxBuf>) -> bool {
    let Some(rx) = LINK_RX.get() else { return false };
    unsafe fn release(ptr: NonNull<u8>, _: usize, _: usize) {
        unsafe { sys::heap_caps_free(ptr.as_ptr().cast()) };
    }
    unsafe fn unhold(_: NonNull<u8>, _: usize, buf: usize) {
        unsafe { &*(buf as *const RxBuf) }.release();
    }
    if let (Some(buf), Some(ptr)) = (held, NonNull::new(frame.as_ptr().cast_mut())) {
        buf.hold();
        rx.deliver(unsafe { RxFrame::from_raw(ptr, frame.len(), buf as *const RxBuf as usize, unhold) });
        return true;
    }
    let buf = unsafe { sys::heap_caps_malloc(frame.len(), sys::MALLOC_CAP_SPIRAM) }.cast::<u8>();
    let Some(buf) = NonNull::new(buf) else { return true };
    unsafe { ptr::copy_nonoverlapping(frame.as_ptr(), buf.as_ptr(), frame.len()) };
    rx.deliver(unsafe { RxFrame::from_raw(buf, frame.len(), 0, release) });
    true
}

pub(super) fn on_connected(bssid: [u8; 6], channel: u8) {
    if let Some(events) = EVENTS.get() {
        events.set_state(WifiState::Connected { bssid, channel });
    }
    UP.store(true, Ordering::Release);
    if let Some(rx) = LINK_RX.get() {
        rx.set_up(true);
    }
}

pub(super) fn on_disconnected(reason: u16) {
    UP.store(false, Ordering::Release);
    if let Some(rx) = LINK_RX.get() {
        rx.set_up(false);
    }
    if let Some(events) = EVENTS.get() {
        events.set_state(WifiState::Disconnected(Some(DisconnectReason::from_code(reason))));
    }
}

/// Runs the blocking RPCs of `f` on a task of their own, so the caller's
/// executor goes on meanwhile.
fn offload<T: Send + 'static>(
    name: &'static str,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> BoxFuture<'static, core::result::Result<T, i32>> {
    let done = Arc::new(Signal::new(&CRITICAL));
    let tx = done.clone();
    let spawned = thread::Builder::new().name(name).stack_size(CONTROL_STACK).spawn(move || {
        let result = f();
        if let Err(err) = &result {
            error!(TAG, "{name}: {err}");
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

fn wait_started(started: bool) -> Result<()> {
    let deadline = Instant::now() + START_WAIT;
    while wifi::sta_started() != started {
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
        let mut init: ffi::wifi_init_config_t = unsafe { mem::zeroed() };
        unsafe { ffi::bsp_rs_wifi_init_default(&mut init) };
        wifi::init(&init)?;
        check(unsafe { wifi::esp_wifi_remote_set_mode(ffi::wifi_mode_t_WIFI_MODE_STA) })?;
        radio.initialized = true;
    }
    check(unsafe { wifi::esp_wifi_remote_start() })?;
    wait_started(true)?;
    check(unsafe { wifi::esp_wifi_remote_get_mac(ffi::wifi_interface_t_WIFI_IF_STA, radio.mac.as_mut_ptr()) })?;
    apply_power_save(radio.power_save)?;
    radio.started = true;
    info!(TAG, "station up, mac {:02x?}", radio.mac);
    Ok(())
}

fn stop_radio() -> Result<()> {
    let mut radio = RADIO.lock();
    if !radio.started {
        return Ok(());
    }
    check(unsafe { wifi::esp_wifi_remote_stop() })?;
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
    check(unsafe { wifi::esp_wifi_remote_scan_start(ptr::null(), true) })?;
    wifi::scan_records(SCAN_MAX, |r| ScanRecord {
        ssid: r.ssid.split(|&b| b == 0).next().unwrap_or_default().to_vec(),
        bssid: r.bssid.try_into().unwrap_or_default(),
        channel: r.primary as u8,
        rssi: r.rssi as i8,
        security: Security::from_auth_mode(r.authmode as u32),
    })
}

struct Owned {
    ssid: Vec<u8>,
    password: Option<Vec<u8>>,
    bssid: Option<[u8; 6]>,
    channel: Option<u8>,
}

fn join(config: &Owned) -> Result<()> {
    let mut sta: ffi::wifi_sta_config_t = unsafe { mem::zeroed() };
    let ssid = &config.ssid[..config.ssid.len().min(sta.ssid.len())];
    sta.ssid[..ssid.len()].copy_from_slice(ssid);
    if let Some(password) = &config.password {
        let password = &password[..password.len().min(sta.password.len())];
        sta.password[..password.len()].copy_from_slice(password);
        sta.threshold.authmode = ffi::wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK;
    }
    sta.scan_method = ffi::wifi_scan_method_t_WIFI_ALL_CHANNEL_SCAN;
    sta.pmf_cfg.capable = true;
    sta.sae_pwe_h2e = ffi::wifi_sae_pwe_method_t_WPA3_SAE_PWE_BOTH;
    if let Some(bssid) = config.bssid {
        sta.bssid_set = true;
        sta.bssid = bssid;
    }
    sta.channel = config.channel.unwrap_or(0);
    wifi::set_config(&sta)?;
    check(unsafe { wifi::esp_wifi_remote_connect() })
}

impl WifiDriver for Hosted {
    fn sta(&self) -> &'static dyn Link {
        &STA
    }

    fn attach(&self, events: WifiEvents) {
        let _ = EVENTS.set(events);
    }

    fn start(&self) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("hosted_start", start_radio)
    }

    fn stop(&self) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("hosted_stop", stop_radio)
    }

    fn scan(&self) -> BoxFuture<'_, core::result::Result<Vec<ScanRecord>, i32>> {
        offload("hosted_scan", scan)
    }

    fn rssi(&self) -> BoxFuture<'_, core::result::Result<i8, i32>> {
        offload("hosted_rssi", || wifi::sta_rssi().map(|rssi| rssi as i8))
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
        offload("hosted_join", move || join(&owned))
    }

    fn set_power_save(&self, mode: PowerSave) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("hosted_ps", move || {
            let mut radio = RADIO.lock();
            radio.power_save = mode;
            if !radio.started {
                return Ok(());
            }
            apply_power_save(mode)
        })
    }

    fn disconnect(&self) -> BoxFuture<'_, core::result::Result<(), i32>> {
        offload("hosted_leave", || {
            if !RADIO.lock().started {
                return Ok(());
            }
            check(unsafe { wifi::esp_wifi_remote_disconnect() })
        })
    }
}
