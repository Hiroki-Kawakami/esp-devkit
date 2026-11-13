/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp-hosted 2.12 compatible coprocessor link over SDIO, behind the IDF
 * esp_wifi_remote API and the NimBLE host transport.
 */

use alloc::boxed::Box;
use core::time::Duration;

use idf::sync::{Mutex, OnceLock};
use idf::{Error, Result};

use crate::driver::sdmmc::Controller;

#[cfg(all(CONFIG_BT_NIMBLE_ENABLED, CONFIG_BT_CONTROLLER_DISABLED, not(CONFIG_BT_NIMBLE_TRANSPORT_UART)))]
mod bt;
mod net;
mod rpc;
mod sdio;
mod transport;
mod wifi;
mod wire;

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

    // bindgen starts the he_* bitfields on a fresh u32 instead of in the one
    // failure_retry_cnt opened, which moves sae_h2e_identifier by one byte.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct wifi_sta_config_t {
        pub ssid: [u8; 32],
        pub password: [u8; 64],
        pub scan_method: wifi_scan_method_t,
        pub bssid_set: bool,
        pub bssid: [u8; 6],
        pub channel: u8,
        pub listen_interval: u16,
        pub sort_method: wifi_sort_method_t,
        pub threshold: wifi_scan_threshold_t,
        pub pmf_cfg: wifi_pmf_config_t,
        /// rm_enabled, btm_enabled, mbo_enabled, ft_enabled, owe_enabled,
        /// transition_disable, disable_wpa3_compatible_mode from bit 0.
        pub flags: u32,
        pub sae_pwe_h2e: wifi_sae_pwe_method_t,
        pub sae_pk_mode: wifi_sae_pk_mode_t,
        pub failure_retry_cnt: u8,
        /// he_dcm_set .. vht_mcs8_enabled from bit 0 of the first three bytes.
        pub he_flags: [u8; 6],
        pub sae_h2e_identifier: [u8; 32],
    }

    const _: () = assert!(core::mem::size_of::<wifi_sta_config_t>() == 184);
    const _: () = assert!(core::mem::offset_of!(wifi_sta_config_t, flags) == 132);
    const _: () = assert!(core::mem::offset_of!(wifi_sta_config_t, failure_retry_cnt) == 144);
    const _: () = assert!(core::mem::offset_of!(wifi_sta_config_t, sae_h2e_identifier) == 151);
}

#[allow(dead_code, non_upper_case_globals)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/protogen/esp_hosted_rpc.pb.rs"));
}

pub(crate) use net::register as register_net;
pub(crate) use sdio::Config;

use proto::{RpcId, RpcPayload};
use transport::{Receivers, Transport};
use wire::If;

const OTA_TIMEOUT: Duration = Duration::from_secs(30);

type TestRx = Box<dyn Fn(&[u8]) + Send + Sync>;

/// The coprocessor link, set up by the first `connect`.
pub(crate) struct EspHosted {
    transport: Transport,
    rpc: rpc::Client,
    test_rx: Mutex<Option<TestRx>>,
}

static CONFIG: OnceLock<(&'static Controller, Config)> = OnceLock::new();
static HOSTED: OnceLock<EspHosted> = OnceLock::new();

/// Board bring-up: where the coprocessor is. Nothing is touched until `connect`.
pub(crate) fn configure(controller: &'static Controller, config: Config) -> Result<()> {
    CONFIG.set((controller, config)).map_err(|_| Error::INVALID_STATE)
}

/// Resets the coprocessor and waits for its handshake, once; later calls
/// return the link.
pub(crate) fn connect() -> Result<&'static EspHosted> {
    let hosted = HOSTED.get_or_try_init(|| {
        let (controller, config) = CONFIG.get().ok_or(Error::INVALID_STATE)?;
        let sdio = sdio::Sdio::new(controller, *config)?;
        Ok::<_, Error>(EspHosted {
            transport: Transport::new(sdio, &DISPATCH)?,
            rpc: rpc::Client::new()?,
            test_rx: Mutex::new(None),
        })
    })?;
    hosted.transport.start()?;
    Ok(hosted)
}

fn linked() -> Result<&'static EspHosted> {
    HOSTED.get().ok_or(Error::INVALID_STATE)
}

struct Dispatch;

static DISPATCH: Dispatch = Dispatch;

impl Receivers for Dispatch {
    fn serial(&self, payload: &[u8], flags: u8) {
        if let Some(hosted) = HOSTED.get() {
            hosted.rpc.on_serial(payload, flags, wifi::on_event);
        }
    }

    fn sta(&self, frame: &[u8], buf: Option<&'static transport::RxBuf>) {
        wifi::on_sta(frame, buf);
    }

    fn hci(&self, payload: &[u8]) {
        #[cfg(all(CONFIG_BT_NIMBLE_ENABLED, CONFIG_BT_CONTROLLER_DISABLED, not(CONFIG_BT_NIMBLE_TRANSPORT_UART)))]
        bt::on_hci(payload);
        #[cfg(not(all(CONFIG_BT_NIMBLE_ENABLED, CONFIG_BT_CONTROLLER_DISABLED, not(CONFIG_BT_NIMBLE_TRANSPORT_UART))))]
        let _ = payload;
    }

    fn test(&self, payload: &[u8]) {
        if let Some(hosted) = HOSTED.get()
            && let Some(rx) = &*hosted.test_rx.lock()
        {
            rx(payload);
        }
    }
}

macro_rules! status_call {
    ($hosted:expr, $id:ident, $req:expr, $resp:ident, $timeout:expr) => {
        $hosted.call($crate::driver::esp_hosted::proto::RpcId::$id, $req, $timeout, |p| match p {
            $crate::driver::esp_hosted::proto::RpcPayload::$resp(l) => Some(
                l.get().map_err(|_| idf::Error::FAIL).and_then(|r| $crate::driver::esp_hosted::rpc::status(r.resp)),
            ),
            _ => None,
        })
    };
}
use status_call;

impl EspHosted {
    fn call<T>(
        &self,
        id: RpcId,
        payload: Option<RpcPayload<'_>>,
        timeout: Duration,
        f: impl FnOnce(&RpcPayload<'_>) -> Option<Result<T>>,
    ) -> Result<T> {
        self.rpc.call(&self.transport, id, payload, timeout, f)
    }
}

#[allow(dead_code)]
impl EspHosted {
    /// `wire::EXT_CAP_*` the coprocessor advertised.
    pub(crate) fn ext_caps(&self) -> u32 {
        self.transport.ext_caps()
    }

    /// major, minor, patch.
    pub(crate) fn fw_version(&self) -> Result<(u32, u32, u32)> {
        let req = proto::RpcReqGetCoprocessorFwVersion::default();
        let payload = RpcPayload::ReqGetCoprocessorFwversion(proto::pb::Lazy::new(&req));
        self.call(RpcId::Req_GetCoprocessorFwVersion, Some(payload), rpc::TIMEOUT, |p| match p {
            RpcPayload::RespGetCoprocessorFwversion(l) => Some(
                l.get()
                    .map_err(|_| Error::FAIL)
                    .and_then(|r| rpc::status(r.resp).map(|()| (r.major1, r.minor1, r.patch1))),
            ),
            _ => None,
        })
    }

    /// The image goes to the coprocessor's inactive OTA slot.
    pub(crate) fn ota_begin(&self) -> Result<()> {
        status_call!(self, Req_OTABegin, None, RespOtaBegin, OTA_TIMEOUT)
    }

    pub(crate) fn ota_write(&self, data: &[u8]) -> Result<()> {
        let req = proto::RpcReqOTAWrite { ota_data: data };
        status_call!(self, Req_OTAWrite, Some(RpcPayload::ReqOtaWrite(proto::pb::Lazy::new(&req))), RespOtaWrite, OTA_TIMEOUT)
    }

    pub(crate) fn ota_end(&self) -> Result<()> {
        status_call!(self, Req_OTAEnd, None, RespOtaEnd, OTA_TIMEOUT)
    }

    /// Switches to the written image; the coprocessor restarts shortly after
    /// replying.
    pub(crate) fn ota_activate(&self) -> Result<()> {
        status_call!(self, Req_OTAActivate, None, RespOtaActivate, rpc::TIMEOUT)
    }

    /// Coprocessor log lines appear on the console prefixed with "[C6]".
    pub(crate) fn set_log_forward(&self, enable: bool) -> Result<()> {
        if self.ext_caps() & wire::EXT_CAP_LOG == 0 {
            return Err(Error::NOT_SUPPORTED);
        }
        let event = [wire::PRIV_EVENT_EXT_CTRL, 3, wire::TLV_CTRL_LOG, 1, u8::from(enable)];
        self.transport.send(If::Priv, 0, &event, None)
    }

    /// `rx` runs on the transport's rx task.
    pub(crate) fn set_test_rx(&self, rx: Option<TestRx>) {
        *self.test_rx.lock() = rx;
    }

    pub(crate) fn test_send(&self, data: &[u8], wait: Duration) -> Result<()> {
        if self.ext_caps() & wire::EXT_CAP_TEST == 0 {
            return Err(Error::NOT_SUPPORTED);
        }
        self.transport.send(If::Test, 0, data, Some(wait))
    }
}
