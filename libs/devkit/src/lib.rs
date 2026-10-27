/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]

mod bitflags;
mod byte_cursor;
mod defer;
mod fmt_buf;
mod static_lease;

pub use byte_cursor::{ByteReader, ByteWriter, OutOfBounds};
pub use defer::{Guard, guard};
pub use fmt_buf::FmtBuf;
pub use static_lease::{Lease, StaticLease};
