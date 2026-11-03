/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod arp;
mod error;
#[cfg(test)]
mod harness;
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
mod wire;

pub use error::{Error, Result};
pub use link::{Link, LinkInfo, LinkRx, Medium, RxFrame, TxError};
pub use net::Config;
pub use stack::{IfaceId, IfaceStats, Ipv4Config, Stack};
pub use tcp_socket::{Shutdown, TcpBuilder, TcpListener, TcpReader, TcpStream, TcpWriter};
pub use time::Instant;
pub use udp_socket::{UdpBuilder, UdpSocket};
