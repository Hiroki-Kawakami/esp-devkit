/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp_wifi_remote over the coprocessor RPCs, plus the esp_wifi internal data
 * hooks that the IDF remote glue declares weak.
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{CStr, c_int, c_void};
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use core::time::Duration;

use idf::log::{info, warn};
use idf::{Error, Result, sys};

use super::ffi;
use super::proto::pb::Lazy;
use super::proto::{self as p, RpcId, RpcPayload};
use super::wire::If;
use super::{EspHosted, rpc, status_call};

const TAG: &CStr = c"esp_hosted_wifi";

const INIT_TIMEOUT: Duration = Duration::from_secs(10);
const SCAN_BLOCK_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_POST_WAIT_MS: u32 = 100;
const TX_WAIT: Duration = Duration::from_millis(20);

const AP_REC_11B: u32 = 0;
const AP_REC_11G: u32 = 1;
const AP_REC_11N: u32 = 2;
const AP_REC_LR: u32 = 3;
const AP_REC_11AX: u32 = 4;
const AP_REC_WPS: u32 = 5;
const AP_REC_FTM_RESPONDER: u32 = 6;
const AP_REC_FTM_INITIATOR: u32 = 7;
const AP_REC_11A: u32 = 8;
const AP_REC_11AC: u32 = 9;

// The bits of wifi_sta_config_t's flags and he_flags that the RPC carries.
const STA_CFG_MASK: u32 = 0x3f;
const STA_HE_MASK: u32 = 0x1fff;

type EspErr = sys::esp_err_t;

static STA_RX: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static STA_STARTED: AtomicBool = AtomicBool::new(false);
static STA_CONNECTED: AtomicBool = AtomicBool::new(false);
static C_API: AtomicBool = AtomicBool::new(false);

fn code(result: Result<()>) -> EspErr {
    result.err().map_or(0, Error::code)
}

fn bit(v: u32, pos: u32) -> u32 {
    (v >> pos) & 1
}

fn linked() -> Result<&'static EspHosted> {
    super::linked()
}

fn copy_bytes(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
}

fn cstr_bytes(s: &[u8]) -> &[u8] {
    &s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_internal_reg_rxcb(ifx: ffi::wifi_interface_t, f: ffi::wifi_rxcb_t) -> EspErr {
    if ifx != ffi::wifi_interface_t_WIFI_IF_STA {
        return Error::NOT_SUPPORTED.code();
    }
    STA_RX.store(f.map_or(ptr::null_mut(), |f| f as *mut c_void), Ordering::Release);
    0
}

pub(super) fn sta_started() -> bool {
    STA_STARTED.load(Ordering::Acquire)
}

pub(super) fn on_sta(frame: &[u8]) {
    let rx = STA_RX.load(Ordering::Acquire);
    if rx.is_null() {
        super::net::on_sta(frame);
        return;
    }
    let rx: unsafe extern "C" fn(*mut c_void, u16, *mut c_void) -> EspErr = unsafe { mem::transmute(rx) };
    let buf = unsafe { sys::heap_caps_malloc(frame.len(), sys::MALLOC_CAP_SPIRAM) };
    if buf.is_null() {
        return;
    }
    unsafe {
        ptr::copy_nonoverlapping(frame.as_ptr(), buf.cast(), frame.len());
        rx(buf, frame.len() as u16, buf);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_internal_free_rx_buffer(buffer: *mut c_void) {
    unsafe { sys::heap_caps_free(buffer) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_internal_tx(ifx: ffi::wifi_interface_t, buffer: *mut c_void, len: u16) -> c_int {
    if ifx != ffi::wifi_interface_t_WIFI_IF_STA {
        return Error::NOT_SUPPORTED.code();
    }
    let frame = unsafe { core::slice::from_raw_parts(buffer.cast::<u8>(), usize::from(len)) };
    code(linked().and_then(|h| h.transport.send(If::Sta, 0, frame, Some(TX_WAIT))))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_internal_tx_by_ref(
    ifx: ffi::wifi_interface_t,
    buffer: *mut c_void,
    len: usize,
    _netstack_buf: *mut c_void,
) -> EspErr {
    unsafe { esp_wifi_internal_tx(ifx, buffer, len as u16) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_internal_reg_netstack_buf_cb(
    _ref: ffi::wifi_netstack_buf_ref_cb_t,
    _free: ffi::wifi_netstack_buf_free_cb_t,
) -> EspErr {
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_internal_set_sta_ip() -> EspErr {
    0
}

fn post<T>(id: u32, data: Option<&T>) {
    if !C_API.load(Ordering::Acquire) {
        return;
    }
    let (data, size) = data.map_or((ptr::null(), 0), |d| (ptr::from_ref(d).cast::<c_void>(), mem::size_of::<T>()));
    let ticks = EVENT_POST_WAIT_MS * sdkconfig::CONFIG_FREERTOS_HZ as u32 / 1000;
    if unsafe { ffi::esp_event_post(ffi::WIFI_EVENT, id as i32, data, size, ticks) } != 0 {
        warn!(TAG, "event {id} dropped");
    }
}

/// On the transport rx task.
pub(super) fn on_event(ev: &p::Rpc<'_>) {
    let Some(payload) = &ev.payload else { return };
    match payload {
        RpcPayload::EventWifiEventNoArgs(l) => {
            let Ok(e) = l.get() else { return };
            let id = e.event_id as u32;
            if id == ffi::wifi_event_t_WIFI_EVENT_STA_START {
                if STA_STARTED.swap(true, Ordering::AcqRel) {
                    return;
                }
            } else if id == ffi::wifi_event_t_WIFI_EVENT_STA_STOP {
                STA_STARTED.store(false, Ordering::Release);
                STA_CONNECTED.store(false, Ordering::Release);
            } else if id != ffi::wifi_event_t_WIFI_EVENT_HOME_CHANNEL_CHANGE {
                return;
            }
            post::<()>(id, None);
        }
        RpcPayload::EventStaConnected(l) => {
            let Ok(e) = l.get() else { return };
            let Some(c) = e.sta_connected else { return };
            if !STA_STARTED.load(Ordering::Acquire) || STA_CONNECTED.load(Ordering::Acquire) {
                return;
            }
            let mut out: ffi::wifi_event_sta_connected_t = unsafe { mem::zeroed() };
            out.ssid_len = c.ssid_len as u8;
            out.channel = c.channel as u8;
            out.authmode = c.authmode as _;
            out.aid = c.aid as u16;
            copy_bytes(&mut out.ssid, c.ssid);
            copy_bytes(&mut out.bssid, c.bssid);
            STA_CONNECTED.store(true, Ordering::Release);
            post(ffi::wifi_event_t_WIFI_EVENT_STA_CONNECTED, Some(&out));
            super::net::on_connected(out.bssid, out.channel);
        }
        RpcPayload::EventStaDisconnected(l) => {
            let Ok(e) = l.get() else { return };
            let Some(d) = e.sta_disconnected else { return };
            let mut out: ffi::wifi_event_sta_disconnected_t = unsafe { mem::zeroed() };
            out.ssid_len = d.ssid_len as u8;
            out.reason = d.reason as u8;
            out.rssi = d.rssi as i8;
            copy_bytes(&mut out.ssid, d.ssid);
            copy_bytes(&mut out.bssid, d.bssid);
            STA_CONNECTED.store(false, Ordering::Release);
            post(ffi::wifi_event_t_WIFI_EVENT_STA_DISCONNECTED, Some(&out));
            super::net::on_disconnected(u16::from(out.reason));
        }
        RpcPayload::EventStaScanDone(l) => {
            let Ok(e) = l.get() else { return };
            let Some(s) = e.scan_done else { return };
            let mut out: ffi::wifi_event_sta_scan_done_t = unsafe { mem::zeroed() };
            out.status = s.status;
            out.number = s.number as u8;
            out.scan_id = s.scan_id as u8;
            post(ffi::wifi_event_t_WIFI_EVENT_SCAN_DONE, Some(&out));
        }
        RpcPayload::EventEspInit(_) => info!(TAG, "coprocessor up"),
        _ => {}
    }
}

pub(super) fn init(c: &ffi::wifi_init_config_t) -> Result<()> {
    let hosted = super::connect()?;
    let cfg = p::WifiInitConfig {
        static_rx_buf_num: c.static_rx_buf_num,
        dynamic_rx_buf_num: c.dynamic_rx_buf_num,
        tx_buf_type: c.tx_buf_type,
        static_tx_buf_num: c.static_tx_buf_num,
        dynamic_tx_buf_num: c.dynamic_tx_buf_num,
        cache_tx_buf_num: c.cache_tx_buf_num,
        csi_enable: c.csi_enable,
        ampdu_rx_enable: c.ampdu_rx_enable,
        ampdu_tx_enable: c.ampdu_tx_enable,
        amsdu_tx_enable: c.amsdu_tx_enable,
        nvs_enable: c.nvs_enable,
        nano_enable: c.nano_enable,
        rx_ba_win: c.rx_ba_win,
        wifi_task_core_id: c.wifi_task_core_id,
        beacon_max_len: c.beacon_max_len,
        mgmt_sbuf_num: c.mgmt_sbuf_num,
        feature_caps: c.feature_caps,
        sta_disconnected_pm: c.sta_disconnected_pm,
        espnow_max_encrypt_num: c.espnow_max_encrypt_num,
        magic: c.magic,
        rx_mgmt_buf_type: c.rx_mgmt_buf_type,
        rx_mgmt_buf_num: c.rx_mgmt_buf_num,
        tx_hetb_queue_num: c.tx_hetb_queue_num,
        dump_hesigb_enable: i32::from(c.dump_hesigb_enable),
    };
    let req = p::RpcReqWifiInit { cfg: Some(Box::new(cfg)) };
    status_call!(hosted, Req_WifiInit, Some(RpcPayload::ReqWifiInit(Lazy::new(&req))), RespWifiInit, INIT_TIMEOUT)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_init(config: *const ffi::wifi_init_config_t) -> EspErr {
    C_API.store(true, Ordering::Release);
    code(init(unsafe { &*config }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_deinit() -> EspErr {
    code(linked().and_then(|h| status_call!(h, Req_WifiDeinit, None, RespWifiDeinit, rpc::TIMEOUT)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn esp_wifi_remote_set_mode(mode: ffi::wifi_mode_t) -> EspErr {
    let req = p::RpcReqSetMode { mode: mode as i32 };
    code(linked().and_then(|h| {
        status_call!(h, Req_SetWifiMode, Some(RpcPayload::ReqSetWifiMode(Lazy::new(&req))), RespSetWifiMode, rpc::TIMEOUT)
    }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_get_mode(mode: *mut ffi::wifi_mode_t) -> EspErr {
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_GetWifiMode, None, rpc::TIMEOUT, |p| match p {
            RpcPayload::RespGetWifiMode(l) => {
                Some(l.get().map_err(|_| Error::FAIL).and_then(|r| rpc::status(r.resp).map(|()| r.mode)))
            }
            _ => None,
        })
    });
    match result {
        Ok(m) => {
            unsafe { *mode = m as ffi::wifi_mode_t };
            0
        }
        Err(err) => err.code(),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_set_storage(storage: ffi::wifi_storage_t) -> EspErr {
    let req = p::RpcReqWifiSetStorage { storage: storage as i32 };
    code(linked().and_then(|h| {
        status_call!(
            h,
            Req_WifiSetStorage,
            Some(RpcPayload::ReqWifiSetStorage(Lazy::new(&req))),
            RespWifiSetStorage,
            rpc::TIMEOUT
        )
    }))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn esp_wifi_remote_get_mac(ifx: ffi::wifi_interface_t, mac: *mut u8) -> EspErr {
    let req = p::RpcReqGetMacAddress { mode: ifx as i32 };
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_GetMACAddress, Some(RpcPayload::ReqGetMacAddress(Lazy::new(&req))), rpc::TIMEOUT, |p| {
            match p {
                RpcPayload::RespGetMacAddress(l) => Some(l.get().map_err(|_| Error::FAIL).and_then(|r| {
                    rpc::status(r.resp)?;
                    <[u8; 6]>::try_from(r.mac).map_err(|_| Error::FAIL)
                })),
                _ => None,
            }
        })
    });
    match result {
        Ok(m) => {
            unsafe { ptr::copy_nonoverlapping(m.as_ptr(), mac, 6) };
            0
        }
        Err(err) => err.code(),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_set_mac(ifx: ffi::wifi_interface_t, mac: *const u8) -> EspErr {
    let req = p::RpcReqSetMacAddress { mode: ifx as i32, mac: unsafe { core::slice::from_raw_parts(mac, 6) } };
    code(linked().and_then(|h| {
        status_call!(
            h,
            Req_SetMacAddress,
            Some(RpcPayload::ReqSetMacAddress(Lazy::new(&req))),
            RespSetMacAddress,
            rpc::TIMEOUT
        )
    }))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn esp_wifi_remote_start() -> EspErr {
    code(linked().and_then(|h| status_call!(h, Req_WifiStart, None, RespWifiStart, rpc::TIMEOUT)))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_stop() -> EspErr {
    code(linked().and_then(|h| status_call!(h, Req_WifiStop, None, RespWifiStop, rpc::TIMEOUT)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn esp_wifi_remote_connect() -> EspErr {
    code(linked().and_then(|h| status_call!(h, Req_WifiConnect, None, RespWifiConnect, rpc::TIMEOUT)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn esp_wifi_remote_disconnect() -> EspErr {
    code(linked().and_then(|h| status_call!(h, Req_WifiDisconnect, None, RespWifiDisconnect, rpc::TIMEOUT)))
}

#[unsafe(no_mangle)]
pub(super) unsafe extern "C" fn esp_wifi_remote_set_ps(ty: ffi::wifi_ps_type_t) -> EspErr {
    let req = p::RpcReqSetPs { r#type: ty as i32 };
    code(linked().and_then(|h| {
        status_call!(h, Req_WifiSetPs, Some(RpcPayload::ReqWifiSetPs(Lazy::new(&req))), RespWifiSetPs, rpc::TIMEOUT)
    }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_get_ps(ty: *mut ffi::wifi_ps_type_t) -> EspErr {
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_WifiGetPs, None, rpc::TIMEOUT, |p| match p {
            RpcPayload::RespWifiGetPs(l) => {
                Some(l.get().map_err(|_| Error::FAIL).and_then(|r| rpc::status(r.resp).map(|()| r.r#type)))
            }
            _ => None,
        })
    });
    match result {
        Ok(t) => {
            unsafe { *ty = t as ffi::wifi_ps_type_t };
            0
        }
        Err(err) => err.code(),
    }
}

pub(super) fn set_config(a: &ffi::wifi_sta_config_t) -> Result<()> {
    let sta = p::WifiStaConfig {
        ssid: cstr_bytes(&a.ssid),
        password: cstr_bytes(&a.password),
        scan_method: a.scan_method as i32,
        bssid_set: a.bssid_set,
        bssid: &a.bssid,
        channel: u32::from(a.channel),
        listen_interval: u32::from(a.listen_interval),
        sort_method: a.sort_method as i32,
        threshold: Some(Box::new(p::WifiScanThreshold {
            rssi: i32::from(a.threshold.rssi),
            authmode: a.threshold.authmode as i32,
            rssi_5g_adjustment: u32::from(a.threshold.rssi_5g_adjustment),
        })),
        pmf_cfg: Some(Box::new(p::WifiPmfConfig { capable: a.pmf_cfg.capable, required: a.pmf_cfg.required })),
        bitmask: a.flags & STA_CFG_MASK,
        sae_pwe_h2e: a.sae_pwe_h2e as i32,
        failure_retry_cnt: u32::from(a.failure_retry_cnt),
        he_bitmask: u32::from_le_bytes([a.he_flags[0], a.he_flags[1], a.he_flags[2], 0]) & STA_HE_MASK,
        sae_h2e_identifier: cstr_bytes(&a.sae_h2e_identifier),
        sae_pk_mode: a.sae_pk_mode as u32,
    };
    let cfg = p::WifiConfig { u: Some(p::WifiConfigU::Sta(Lazy::new(&sta))) };
    let req = p::RpcReqWifiSetConfig { iface: ffi::wifi_interface_t_WIFI_IF_STA as i32, cfg: Some(Box::new(cfg)) };
    let hosted = linked()?;
    status_call!(
        hosted,
        Req_WifiSetConfig,
        Some(RpcPayload::ReqWifiSetConfig(Lazy::new(&req))),
        RespWifiSetConfig,
        rpc::TIMEOUT
    )
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_set_config(ifx: ffi::wifi_interface_t, conf: *mut ffi::wifi_config_t) -> EspErr {
    if ifx != ffi::wifi_interface_t_WIFI_IF_STA {
        return Error::NOT_SUPPORTED.code();
    }
    code(set_config(unsafe { (*conf).sta.as_ref() }))
}

fn sta_config_from(c: &p::WifiStaConfig<'_>, a: &mut ffi::wifi_sta_config_t) {
    copy_bytes(&mut a.ssid, c.ssid);
    copy_bytes(&mut a.password, c.password);
    a.scan_method = c.scan_method as _;
    a.bssid_set = c.bssid_set;
    copy_bytes(&mut a.bssid, c.bssid);
    a.channel = c.channel as u8;
    a.listen_interval = c.listen_interval as u16;
    a.sort_method = c.sort_method as _;
    if let Some(t) = &c.threshold {
        a.threshold.rssi = t.rssi as i8;
        a.threshold.authmode = t.authmode as _;
        a.threshold.rssi_5g_adjustment = t.rssi_5g_adjustment as u8;
    }
    if let Some(m) = &c.pmf_cfg {
        a.pmf_cfg.capable = m.capable;
        a.pmf_cfg.required = m.required;
    }
    a.flags = c.bitmask & STA_CFG_MASK;
    a.sae_pwe_h2e = c.sae_pwe_h2e as _;
    a.sae_pk_mode = c.sae_pk_mode as _;
    a.failure_retry_cnt = c.failure_retry_cnt as u8;
    a.he_flags[..3].copy_from_slice(&(c.he_bitmask & STA_HE_MASK).to_le_bytes()[..3]);
    copy_bytes(&mut a.sae_h2e_identifier, c.sae_h2e_identifier);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_get_config(ifx: ffi::wifi_interface_t, conf: *mut ffi::wifi_config_t) -> EspErr {
    if ifx != ffi::wifi_interface_t_WIFI_IF_STA {
        return Error::NOT_SUPPORTED.code();
    }
    let req = p::RpcReqWifiGetConfig { iface: ifx as i32 };
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_WifiGetConfig, Some(RpcPayload::ReqWifiGetConfig(Lazy::new(&req))), rpc::TIMEOUT, |p| {
            let RpcPayload::RespWifiGetConfig(l) = p else { return None };
            Some(l.get().map_err(|_| Error::FAIL).and_then(|r| {
                rpc::status(r.resp)?;
                let Some(p::WifiConfigU::Sta(sta)) = r.cfg.and_then(|c| c.u) else { return Err(Error::FAIL) };
                let sta = sta.get().map_err(|_| Error::FAIL)?;
                let mut out: ffi::wifi_config_t = unsafe { mem::zeroed() };
                sta_config_from(&sta, unsafe { out.sta.as_mut() });
                Ok(out)
            }))
        })
    });
    match result {
        Ok(out) => {
            unsafe { *conf = out };
            0
        }
        Err(err) => err.code(),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_scan_start(config: *const ffi::wifi_scan_config_t, block: bool) -> EspErr {
    let config = unsafe { config.as_ref() };
    let sc = config.map(|c| p::WifiScanConfig {
        ssid: if c.ssid.is_null() { &[] } else { cstr_bytes(unsafe { core::slice::from_raw_parts(c.ssid, 32) }) },
        bssid: if c.bssid.is_null() { &[] } else { unsafe { core::slice::from_raw_parts(c.bssid, 6) } },
        channel: u32::from(c.channel),
        show_hidden: c.show_hidden,
        scan_type: c.scan_type as i32,
        scan_time: Some(Box::new(p::WifiScanTime {
            active: Some(Box::new(p::WifiActiveScanTime { min: c.scan_time.active.min, max: c.scan_time.active.max })),
            passive: c.scan_time.passive,
        })),
        home_chan_dwell_time: u32::from(c.home_chan_dwell_time),
        channel_bitmap: Some(Box::new(p::WifiScanChannelBitmap {
            ghz_2_channels: u32::from(c.channel_bitmap.ghz_2_channels),
            ghz_5_channels: c.channel_bitmap.ghz_5_channels,
        })),
    });
    let config_set = i32::from(sc.is_some());
    let req = p::RpcReqWifiScanStart { config: sc.map(Box::new), block, config_set };
    let timeout = if block { SCAN_BLOCK_TIMEOUT } else { rpc::TIMEOUT };
    code(linked().and_then(|h| {
        status_call!(h, Req_WifiScanStart, Some(RpcPayload::ReqWifiScanStart(Lazy::new(&req))), RespWifiScanStart, timeout)
    }))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_scan_stop() -> EspErr {
    code(linked().and_then(|h| status_call!(h, Req_WifiScanStop, None, RespWifiScanStop, rpc::TIMEOUT)))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_scan_get_ap_num(number: *mut u16) -> EspErr {
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_WifiScanGetApNum, None, rpc::TIMEOUT, |p| match p {
            RpcPayload::RespWifiScanGetApNum(l) => {
                Some(l.get().map_err(|_| Error::FAIL).and_then(|r| rpc::status(r.resp).map(|()| r.number)))
            }
            _ => None,
        })
    });
    match result {
        Ok(n) => {
            unsafe { *number = n as u16 };
            0
        }
        Err(err) => err.code(),
    }
}

fn ap_record_from(r: &p::WifiApRecord<'_>) -> ffi::wifi_ap_record_t {
    let mut a: ffi::wifi_ap_record_t = unsafe { mem::zeroed() };
    copy_bytes(&mut a.bssid, r.bssid);
    let ssid_cap = a.ssid.len() - 1;
    copy_bytes(&mut a.ssid[..ssid_cap], r.ssid);
    a.primary = r.primary as u8;
    a.second = r.second as _;
    a.rssi = r.rssi as i8;
    a.authmode = r.authmode as _;
    a.pairwise_cipher = r.pairwise_cipher as _;
    a.group_cipher = r.group_cipher as _;
    a.ant = r.ant as _;
    a.set_phy_11b(bit(r.bitmask, AP_REC_11B));
    a.set_phy_11g(bit(r.bitmask, AP_REC_11G));
    a.set_phy_11n(bit(r.bitmask, AP_REC_11N));
    a.set_phy_lr(bit(r.bitmask, AP_REC_LR));
    a.set_phy_11ax(bit(r.bitmask, AP_REC_11AX));
    a.set_wps(bit(r.bitmask, AP_REC_WPS));
    a.set_ftm_responder(bit(r.bitmask, AP_REC_FTM_RESPONDER));
    a.set_ftm_initiator(bit(r.bitmask, AP_REC_FTM_INITIATOR));
    a.set_phy_11a(bit(r.bitmask, AP_REC_11A));
    a.set_phy_11ac(bit(r.bitmask, AP_REC_11AC));
    if let Some(c) = &r.country {
        copy_bytes(as_u8_mut(&mut a.country.cc), c.cc);
        a.country.schan = c.schan as u8;
        a.country.nchan = c.nchan as u8;
        a.country.max_tx_power = c.max_tx_power as i8;
        a.country.policy = c.policy as _;
    }
    if let Some(h) = &r.he_ap {
        a.he_ap.set_bss_color((h.bitmask & 0x3f) as u8);
        a.he_ap.set_partial_bss_color(bit(h.bitmask, 6) as u8);
        a.he_ap.set_bss_color_disabled(bit(h.bitmask, 7) as u8);
        a.he_ap.bssid_index = h.bssid_index as u8;
    }
    a.bandwidth = r.bandwidth as _;
    a.vht_ch_freq1 = r.vht_ch_freq1 as u8;
    a.vht_ch_freq2 = r.vht_ch_freq2 as u8;
    a
}

fn as_u8_mut(s: &mut [core::ffi::c_char]) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(s.as_mut_ptr().cast(), s.len()) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_scan_get_ap_records(number: *mut u16, records: *mut ffi::wifi_ap_record_t) -> EspErr {
    let req = p::RpcReqWifiScanGetApRecords { number: i32::from(unsafe { *number }) };
    let max = usize::from(unsafe { *number });
    let result = linked().and_then(|h| {
        h.call(
            RpcId::Req_WifiScanGetApRecords,
            Some(RpcPayload::ReqWifiScanGetApRecords(Lazy::new(&req))),
            rpc::TIMEOUT,
            |p| {
                let RpcPayload::RespWifiScanGetApRecords(l) = p else { return None };
                Some(l.get().map_err(|_| Error::FAIL).and_then(|r| {
                    rpc::status(r.resp)?;
                    Ok(r.ap_records.iter().take(max).map(ap_record_from).collect::<Vec<_>>())
                }))
            },
        )
    });
    match result {
        Ok(list) => {
            unsafe {
                ptr::copy_nonoverlapping(list.as_ptr(), records, list.len());
                *number = list.len() as u16;
            }
            0
        }
        Err(err) => err.code(),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_sta_get_ap_info(ap_info: *mut ffi::wifi_ap_record_t) -> EspErr {
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_WifiStaGetApInfo, None, rpc::TIMEOUT, |p| {
            let RpcPayload::RespWifiStaGetApInfo(l) = p else { return None };
            Some(l.get().map_err(|_| Error::FAIL).and_then(|r| {
                rpc::status(r.resp)?;
                r.ap_record.as_deref().map(ap_record_from).ok_or(Error::FAIL)
            }))
        })
    });
    match result {
        Ok(info) => {
            unsafe { *ap_info = info };
            0
        }
        Err(err) => err.code(),
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn esp_wifi_remote_sta_get_rssi(rssi: *mut c_int) -> EspErr {
    let result = linked().and_then(|h| {
        h.call(RpcId::Req_WifiStaGetRssi, None, rpc::TIMEOUT, |p| match p {
            RpcPayload::RespWifiStaGetRssi(l) => {
                Some(l.get().map_err(|_| Error::FAIL).and_then(|r| rpc::status(r.resp).map(|()| r.rssi)))
            }
            _ => None,
        })
    });
    match result {
        Ok(v) => {
            unsafe { *rssi = v };
            0
        }
        Err(err) => err.code(),
    }
}
