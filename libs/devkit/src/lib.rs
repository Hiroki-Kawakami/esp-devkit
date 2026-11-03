/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![feature(core_float_math)]

extern crate alloc;

mod align;
mod bitflags;
mod byte_cursor;
mod defer;
pub mod executor;
mod fmt_buf;
pub mod math;
mod static_lease;
pub mod sync;
mod zeroable;

pub use align::A16;
pub use byte_cursor::{ByteReader, ByteWriter, OutOfBounds};
pub use defer::{Guard, guard};
pub use fmt_buf::FmtBuf;
pub use static_lease::{Lease, StaticLease};
pub use zeroable::Zeroable;
