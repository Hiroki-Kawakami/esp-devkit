/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_SOC_* and CONFIG_DEVKIT_NET_WIFI exist only where the chip has Wi-Fi.
#![allow(unexpected_cfgs)]

extern crate alloc;

mod addr;
mod arp;
mod dhcp;
pub mod dns;
mod error;
mod frag;
#[cfg(test)]
mod harness;
mod iface;
mod icmp;
mod link;
mod net;
mod ring;
#[cfg(all(not(target_os = "none"), not(test)))]
mod sim;
mod stack;
mod tcp;
mod tcp_socket;
#[cfg(test)]
mod tests;
mod time;
mod tls;
mod udp;
mod udp_socket;
mod watch;
mod wifi;
pub mod wifi_manager;
#[cfg(CONFIG_DEVKIT_NET_WIFI)]
mod wifi_native;
mod wire;

pub use addr::ToSocketAddrs;
pub use error::{Error, Result};
pub use link::{Link, LinkInfo, LinkRx, Medium, RxFrame, TxError};
pub use iface::{Interface, add_link, interfaces, start};
pub use net::{Config, Ipv4Mode};
pub use stack::{IfaceId, IfaceStats, Ipv4Config, Stack};
pub use tcp_socket::{Shutdown, TcpBuilder, TcpListener, TcpReadLock, TcpReader, TcpStream, TcpWriter};
pub use time::Instant;
pub use tls::{TlsReader, TlsStream, TlsWriter};
pub use udp_socket::{UdpBuilder, UdpSocket};
pub use wifi::{
    Auth, BoxFuture, ConnectError, DisconnectReason, PowerSave, ScanRecord, Security, StaConfig, Wifi, WifiDriver,
    WifiEvents, WifiState, register_wifi, wifi,
};
