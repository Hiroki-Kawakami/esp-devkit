/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod arp;
mod dhcp;
mod error;
#[cfg(test)]
mod harness;
mod iface;
mod link;
mod net;
mod ring;
mod stack;
mod tcp;
mod tcp_socket;
#[cfg(test)]
mod tests;
mod time;
mod udp;
mod udp_socket;
mod wifi;
mod wire;

pub use error::{Error, Result};
pub use link::{Link, LinkInfo, LinkRx, Medium, RxFrame, TxError};
pub use iface::{Interface, add_link, interfaces, start};
pub use net::{Config, Ipv4Mode};
pub use stack::{IfaceId, IfaceStats, Ipv4Config, Stack};
pub use tcp_socket::{Shutdown, TcpBuilder, TcpListener, TcpReader, TcpStream, TcpWriter};
pub use time::Instant;
pub use udp_socket::{UdpBuilder, UdpSocket};
pub use wifi::{
    Auth, BoxFuture, ConnectError, DisconnectReason, StaConfig, Wifi, WifiDriver, WifiEvents, WifiState, register_wifi, wifi,
};
