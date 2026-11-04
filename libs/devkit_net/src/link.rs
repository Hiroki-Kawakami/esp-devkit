/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ops::Deref;
use core::ptr::NonNull;
use core::slice;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Medium {
    Ethernet,
    /// Frames are bare IP packets.
    Ip,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LinkInfo {
    pub medium: Medium,
    /// Ignored on `Medium::Ip`.
    pub mac: [u8; 6],
    /// Largest IP packet, without the Ethernet header.
    pub mtu: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxError {
    Down,
    Busy,
}

pub trait Link: Send + Sync {
    fn info(&self) -> LinkInfo;
    /// Called once, before the first `transmit`. Received frames and the
    /// link state go to `rx` from then on.
    fn attach(&self, rx: LinkRx);
    /// `frame` is only borrowed for the call. May block until the link has
    /// room, which stalls the whole stack meanwhile. After a `Busy`, a link
    /// that can tell calls `LinkRx::tx_ready` once it has room again; senders
    /// waiting on the others retry on a short timer.
    fn transmit(&self, frame: &[u8]) -> Result<(), TxError>;
}

pub(crate) trait RxSink: Send + Sync {
    fn deliver(&self, frame: RxFrame) -> bool;
    fn set_up(&self, up: bool);
    fn tx_ready(&self);
}

/// The stack's side of a link, for the link's own tasks.
#[derive(Clone)]
pub struct LinkRx(Arc<dyn RxSink>);

impl LinkRx {
    pub(crate) fn new(sink: Arc<dyn RxSink>) -> Self {
        Self(sink)
    }

    /// Not from an interrupt. `false` when the stack's queue was full and the
    /// frame was dropped.
    pub fn deliver(&self, frame: RxFrame) -> bool {
        self.0.deliver(frame)
    }

    /// The link starts down.
    pub fn set_up(&self, up: bool) {
        self.0.set_up(up);
    }

    /// Not from an interrupt. Cheap when nobody waits, so it may be called
    /// for every frame sent.
    #[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
    pub fn tx_ready(&self) {
        self.0.tx_ready();
    }
}

/// A received frame in memory the link lends to the stack. The stack drops
/// it as soon as it has been processed.
pub struct RxFrame {
    ptr: NonNull<u8>,
    len: usize,
    ctx: usize,
    release: unsafe fn(NonNull<u8>, usize, usize),
}

unsafe impl Send for RxFrame {}

impl RxFrame {
    /// # Safety
    /// `len` bytes at `ptr` stay valid and unchanged until
    /// `release(ptr, len, ctx)`, which runs once, on whichever task drops
    /// the frame.
    pub unsafe fn from_raw(ptr: NonNull<u8>, len: usize, ctx: usize, release: unsafe fn(NonNull<u8>, usize, usize)) -> Self {
        Self { ptr, len, ctx, release }
    }
}

impl From<Box<[u8]>> for RxFrame {
    fn from(data: Box<[u8]>) -> Self {
        unsafe fn free(ptr: NonNull<u8>, len: usize, _: usize) {
            drop(unsafe { Box::from_raw(core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), len)) });
        }
        let len = data.len();
        let ptr = NonNull::new(Box::into_raw(data).cast::<u8>()).unwrap();
        unsafe { Self::from_raw(ptr, len, 0, free) }
    }
}

impl Deref for RxFrame {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for RxFrame {
    fn drop(&mut self) {
        unsafe { (self.release)(self.ptr, self.len, self.ctx) };
    }
}
