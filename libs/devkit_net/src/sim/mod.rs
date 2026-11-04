/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator networking: a radio that joins any network at once. Its station
 * exchanges Ethernet frames with the simulator host, which runs libslirp.
 */

use alloc::boxed::Box;
use alloc::vec;
use core::ffi::CStr;

use idf::log::{error, warn};
use idf::sync::OnceLock;
use idf::thread;

use crate::link::{Link, LinkInfo, LinkRx, Medium, RxFrame, TxError};
use crate::wifi::{BoxFuture, DisconnectReason, PowerSave, StaConfig, WifiDriver, WifiEvents, WifiState};

mod host;

const TAG: &CStr = c"devkit_net_sim";

/// A datagram socket to the host, one Ethernet frame per datagram.
const ENV_NET_FD: &CStr = c"SIM_NET_FD";
const MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x15];
const BSSID: [u8; 6] = [0x02, 0, 0, 0, 0, 0x02];
const MTU: usize = 1500;
const FRAME_MAX: usize = MTU + 14;
const RX_STACK: usize = 8192;

struct Sta;
struct Radio;

static STA: Sta = Sta;
static RADIO: Radio = Radio;
static FD: OnceLock<Option<host::Fd>> = OnceLock::new();
static LINK_RX: OnceLock<LinkRx> = OnceLock::new();
static EVENTS: OnceLock<WifiEvents> = OnceLock::new();

pub(crate) fn register() {
    crate::wifi::register_wifi(&RADIO);
}

fn fd() -> Option<host::Fd> {
    *FD.get_or_init(|| {
        let fd = host::env_fd(ENV_NET_FD);
        if fd.is_none() {
            warn!(TAG, "no simulator host network");
        }
        fd
    })
}

fn receive(fd: host::Fd, rx: &LinkRx) {
    let mut buf = vec![0u8; FRAME_MAX];
    while let Some(n) = host::recv(fd, &mut buf) {
        if n > 0 {
            rx.deliver(RxFrame::from(buf[..n].to_vec().into_boxed_slice()));
        }
    }
    error!(TAG, "host network closed");
}

impl Link for Sta {
    fn info(&self) -> LinkInfo {
        LinkInfo { medium: Medium::Ethernet, mac: MAC, mtu: MTU }
    }

    fn attach(&self, rx: LinkRx) {
        let (Some(fd), Ok(())) = (fd(), LINK_RX.set(rx)) else { return };
        let spawned = thread::Builder::new()
            .name("sim_net_rx")
            .stack_size(RX_STACK)
            .spawn(move || receive(fd, LINK_RX.get().unwrap()));
        if let Err(err) = spawned {
            error!(TAG, "rx task: {err}");
        }
    }

    fn transmit(&self, frame: &[u8]) -> Result<(), TxError> {
        let fd = fd().ok_or(TxError::Down)?;
        if host::send(fd, frame) { Ok(()) } else { Err(TxError::Busy) }
    }
}

impl WifiDriver for Radio {
    fn sta(&self) -> &'static dyn Link {
        &STA
    }

    fn attach(&self, events: WifiEvents) {
        let _ = EVENTS.set(events);
    }

    fn connect<'a>(&'a self, _: &'a StaConfig<'a>) -> BoxFuture<'a, Result<(), i32>> {
        Box::pin(async {
            if fd().is_none() {
                return Err(idf::Error::NOT_SUPPORTED.code());
            }
            if let Some(events) = EVENTS.get() {
                events.set_state(WifiState::Connected { bssid: BSSID, channel: 1 });
            }
            if let Some(rx) = LINK_RX.get() {
                rx.set_up(true);
            }
            Ok(())
        })
    }

    fn set_power_save(&self, _: PowerSave) -> BoxFuture<'_, Result<(), i32>> {
        Box::pin(async { Ok(()) })
    }

    fn disconnect(&self) -> BoxFuture<'_, Result<(), i32>> {
        Box::pin(async {
            if let Some(rx) = LINK_RX.get() {
                rx.set_up(false);
            }
            if let Some(events) = EVENTS.get() {
                events.set_state(WifiState::Disconnected(Some(DisconnectReason::Requested)));
            }
            Ok(())
        })
    }
}
