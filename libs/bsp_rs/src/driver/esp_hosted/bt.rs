/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * NimBLE host transport: HCI over the coprocessor's HCI interface.
 */

use core::ffi::{CStr, c_int, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use idf::log::{error, warn};
use idf::time::Instant;
use idf::{Error, Result};

use super::proto::pb::Lazy;
use super::proto::{self as p, RpcPayload};
use super::wire::{self, If};
use super::{EspHosted, rpc, status_call};

const TAG: &CStr = c"esp_hosted_bt";

const TX_WAIT: Duration = Duration::from_secs(1);
const ACL_WAIT: Duration = Duration::from_millis(100);

const BLE_ERR_MEM_CAPACITY: c_int = 0x07;
const EVCODE_LE_META: u8 = 0x3e;
const SUBEV_ADV_RPT: u8 = 0x02;
const SUBEV_EXT_ADV_RPT: u8 = 0x0d;
#[cfg(CONFIG_BT_NIMBLE_MEMPOOL_RUNTIME_ALLOC)]
const BLE_HCI_CMD: u8 = 1;

#[repr(C)]
struct OsMbuf {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn ble_transport_alloc_evt(discardable: c_int) -> *mut c_void;
    fn ble_transport_alloc_acl_from_ll() -> *mut OsMbuf;
    #[cfg(CONFIG_BT_NIMBLE_MEMPOOL_RUNTIME_ALLOC)]
    fn ble_transport_free(ty: u8, buf: *mut c_void);
    #[cfg(not(CONFIG_BT_NIMBLE_MEMPOOL_RUNTIME_ALLOC))]
    fn ble_transport_free(buf: *mut c_void);
    // ble_transport_to_hs_*() are static inline over these unless the BLE
    // monitor is built in.
    fn ble_transport_to_hs_evt_impl(buf: *mut c_void) -> c_int;
    fn ble_transport_to_hs_acl_impl(om: *mut OsMbuf) -> c_int;
    fn os_mbuf_len(om: *const OsMbuf) -> u16;
    fn os_mbuf_copydata(om: *const OsMbuf, off: c_int, len: c_int, dst: *mut c_void) -> c_int;
    fn os_mbuf_append(om: *mut OsMbuf, data: *const c_void, len: u16) -> c_int;
    fn os_mbuf_free_chain(om: *mut OsMbuf) -> c_int;
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static CONTROLLED: AtomicBool = AtomicBool::new(false);

fn feature_control(hosted: &EspHosted, command: p::RpcFeatureCommand) -> Result<()> {
    let req = p::RpcReqFeatureControl {
        feature: p::RpcFeature::Feature_Bluetooth,
        command,
        option: p::RpcFeatureOption::Feature_Option_None,
    };
    status_call!(
        hosted,
        Req_FeatureControl,
        Some(RpcPayload::ReqFeatureControl(Lazy::new(&req))),
        RespFeatureControl,
        rpc::TIMEOUT
    )
}

// Firmware 1.x enables its controller at boot and has no FeatureControl.
fn controller_up(hosted: &EspHosted) -> Result<()> {
    let (major, _, _) = hosted.fw_version()?;
    let controlled = major >= 2;
    CONTROLLED.store(controlled, Ordering::Release);
    if !controlled {
        return Ok(());
    }
    feature_control(hosted, p::RpcFeatureCommand::Feature_Command_BT_Init)?;
    feature_control(hosted, p::RpcFeatureCommand::Feature_Command_BT_Enable)
}

fn ll_init() -> Result<()> {
    let hosted = super::connect()?;
    if hosted.transport.caps() & wire::CAP_BT_SDIO == 0 {
        return Err(Error::NOT_SUPPORTED);
    }
    controller_up(hosted)
}

#[unsafe(no_mangle)]
extern "C" fn ble_transport_ll_init() {
    match ll_init() {
        Ok(()) => ENABLED.store(true, Ordering::Release),
        Err(err) => error!(TAG, "coprocessor controller: {err}"),
    }
}

#[unsafe(no_mangle)]
extern "C" fn ble_transport_ll_deinit() {
    if !ENABLED.swap(false, Ordering::AcqRel) || !CONTROLLED.load(Ordering::Acquire) {
        return;
    }
    let result = super::linked().and_then(|hosted| {
        feature_control(hosted, p::RpcFeatureCommand::Feature_Command_BT_Disable)?;
        feature_control(hosted, p::RpcFeatureCommand::Feature_Command_BT_Deinit)
    });
    if let Err(err) = result {
        error!(TAG, "coprocessor controller off: {err}");
    }
}

fn send(pkt_type: u8, len: usize, fill: impl FnOnce(&mut [u8])) -> bool {
    if !ENABLED.load(Ordering::Acquire) || len > wire::MAX_PAYLOAD {
        return false;
    }
    let Ok(hosted) = super::linked() else { return false };
    let Some(mut buf) = hosted.transport.tx_get(If::Hci, Some(TX_WAIT)) else { return false };
    fill(&mut buf.payload()[..len]);
    hosted.transport.tx_put(buf, If::Hci, 0, pkt_type, len);
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ble_transport_to_ll_cmd_impl(buf: *mut c_void) -> c_int {
    let cmd = buf.cast::<u8>();
    let len = 3 + usize::from(unsafe { *cmd.add(2) });
    let sent = send(wire::HCI_CMD, len, |out| unsafe { ptr::copy_nonoverlapping(cmd, out.as_mut_ptr(), len) });
    if !sent {
        error!(TAG, "cmd 0x{:04x} not sent", u16::from_le_bytes(unsafe { [*cmd, *cmd.add(1)] }));
    }
    #[cfg(CONFIG_BT_NIMBLE_MEMPOOL_RUNTIME_ALLOC)]
    unsafe {
        ble_transport_free(BLE_HCI_CMD, buf)
    };
    #[cfg(not(CONFIG_BT_NIMBLE_MEMPOOL_RUNTIME_ALLOC))]
    unsafe {
        ble_transport_free(buf)
    };
    if sent { 0 } else { BLE_ERR_MEM_CAPACITY }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ble_transport_to_ll_acl_impl(om: *mut OsMbuf) -> c_int {
    let len = usize::from(unsafe { os_mbuf_len(om) });
    let sent = send(wire::HCI_ACL, len, |out| unsafe {
        os_mbuf_copydata(om, 0, len as c_int, out.as_mut_ptr().cast());
    });
    if !sent {
        error!(TAG, "acl len {len} not sent");
    }
    unsafe { os_mbuf_free_chain(om) };
    if sent { 0 } else { BLE_ERR_MEM_CAPACITY }
}

fn evt_to_host(ev: &[u8]) {
    if ev.len() < 2 || ev.len() != 2 + usize::from(ev[1]) || ev.len() > sdkconfig::CONFIG_BT_NIMBLE_TRANSPORT_EVT_SIZE as usize {
        warn!(TAG, "malformed event, {} bytes", ev.len());
        return;
    }
    let adv = ev[0] == EVCODE_LE_META && ev.len() > 2 && (ev[2] == SUBEV_ADV_RPT || ev[2] == SUBEV_EXT_ADV_RPT);
    let buf = unsafe { ble_transport_alloc_evt(c_int::from(adv)) };
    if buf.is_null() {
        if !adv {
            error!(TAG, "no event buffer, event 0x{:02x} dropped", ev[0]);
        }
        return;
    }
    unsafe {
        ptr::copy_nonoverlapping(ev.as_ptr(), buf.cast(), ev.len());
        ble_transport_to_hs_evt_impl(buf);
    }
}

// Waiting here stalls the whole link through the coprocessor's tokens; the
// bound keeps a host task that waits on an HCI event from deadlocking it.
fn acl_to_host(data: &[u8]) {
    let deadline = Instant::now() + ACL_WAIT;
    let om = loop {
        let om = unsafe { ble_transport_alloc_acl_from_ll() };
        if !om.is_null() {
            break om;
        }
        if !ENABLED.load(Ordering::Acquire) || Instant::now() >= deadline {
            error!(TAG, "no acl buffer, {} bytes dropped", data.len());
            return;
        }
        unsafe { idf::sys::vTaskDelay(1) };
    };
    if unsafe { os_mbuf_append(om, data.as_ptr().cast(), data.len() as u16) } != 0 {
        error!(TAG, "acl of {} bytes does not fit", data.len());
        unsafe { os_mbuf_free_chain(om) };
        return;
    }
    unsafe { ble_transport_to_hs_acl_impl(om) };
}

/// On the transport rx task.
pub(super) fn on_hci(payload: &[u8]) {
    if !ENABLED.load(Ordering::Acquire) || payload.len() < 2 {
        return;
    }
    match payload[0] {
        wire::HCI_EVT => evt_to_host(&payload[1..]),
        wire::HCI_ACL => acl_to_host(&payload[1..]),
        _ => {}
    }
}
