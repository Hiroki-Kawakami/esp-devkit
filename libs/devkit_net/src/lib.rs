/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod arp;
#[cfg(test)]
mod harness;
mod link;
mod stack;
#[cfg(test)]
mod tests;
mod time;
mod wire;

pub use link::{Link, LinkInfo, LinkRx, Medium, RxFrame, TxError};
pub use stack::{IfaceId, IfaceStats, Ipv4Config, Stack};
pub use time::Instant;
