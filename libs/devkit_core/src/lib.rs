/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![feature(core_float_math)]

extern crate alloc;

mod align;
mod bitflags;
mod boxed;
mod buffer_pool;
mod byte_cursor;
mod crc32;
mod defer;
pub mod executor;
mod fmt_buf;
pub mod math;
pub mod sync;
mod zeroable;

pub use align::A16;
#[doc(hidden)]
pub use boxed::__Box;
pub use buffer_pool::{BufferBytes, BufferLease, BufferPool};
pub use byte_cursor::{ByteReader, ByteWriter, OutOfBounds};
pub use crc32::Crc32;
pub use defer::{Guard, guard};
pub use fmt_buf::FmtBuf;
pub use zeroable::Zeroable;
