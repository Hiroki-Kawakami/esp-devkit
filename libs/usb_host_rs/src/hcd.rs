/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The interface between the host core and a host controller.
 */

use alloc::sync::Arc;
use core::cell::UnsafeCell;

use idf::executor::{CRITICAL, Sender, Signal};

use crate::desc::TransferType;
use crate::{Buffer, Result, Setup};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    Low,
    Full,
    High,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PortEvent {
    Connected,
    Disconnected,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) struct PipeConfig {
    pub kind: TransferType,
    pub endpoint: u8,
    pub max_packet_bytes: u16,
    pub interval: u8,
    pub address: u8,
    pub speed: Speed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PipeId(pub u16);

/// One transfer. The controller owns the buffer from `submit` until it
/// calls `complete`, and never touches it afterwards.
pub(crate) struct Request {
    pub setup: Option<Setup>,
    pub endpoint: u8,
    /// Into the buffer, on a cache line.
    pub offset: usize,
    pub len: usize,
    /// OUT: end with a zero-length packet when `len` is a multiple of the
    /// max packet size.
    pub zlp: bool,
    buffer: UnsafeCell<Option<Buffer>>,
    done: Signal<Result<usize>>,
    /// The controller's, under its lock: the next request in a list.
    #[allow(dead_code)]
    pub link: UnsafeCell<Option<Arc<Request>>>,
    /// The controller's, under its lock: the result `complete` is to give.
    #[allow(dead_code)]
    pub outcome: UnsafeCell<Option<Result<usize>>>,
}

unsafe impl Send for Request {}
unsafe impl Sync for Request {}

impl Request {
    pub fn new(setup: Option<Setup>, endpoint: u8, buffer: Buffer, offset: usize, len: usize, zlp: bool) -> Arc<Self> {
        Arc::new(Self {
            setup,
            endpoint,
            offset,
            len,
            zlp,
            buffer: UnsafeCell::new(Some(buffer)),
            done: Signal::new(&CRITICAL),
            link: UnsafeCell::new(None),
            outcome: UnsafeCell::new(None),
        })
    }

    pub fn is_in(&self) -> bool {
        match self.setup {
            Some(setup) => setup.is_in(),
            None => self.endpoint & 0x80 != 0,
        }
    }

    /// # Safety
    /// Only the controller, between `submit` and `complete`.
    #[allow(clippy::mut_from_ref, dead_code)]
    pub unsafe fn data(&self) -> &mut [u8] {
        let buffer = unsafe { (*self.buffer.get()).as_mut().unwrap() };
        &mut buffer[self.offset..self.offset + self.len]
    }

    /// The allocation from `offset` on, cache lines included: what the
    /// controller may write. Same safety as `data`.
    #[allow(dead_code)]
    pub unsafe fn dma(&self) -> (*mut u8, usize) {
        let (ptr, capacity) = unsafe { (*self.buffer.get()).as_ref().unwrap().dma() };
        (unsafe { ptr.add(self.offset) }, capacity - self.offset)
    }

    pub fn complete(&self, result: Result<usize>) {
        self.done.signal(result);
    }

    async fn wait(&self) -> Result<usize> {
        self.done.wait().await
    }

    fn take_buffer(&self) -> Buffer {
        unsafe { (*self.buffer.get()).take().unwrap() }
    }
}

pub(crate) trait Hcd: Sync {
    fn init(&'static self, events: Sender<PortEvent>) -> Result<()>;
    async fn reset(&self) -> Result<Speed>;
    fn pipe_alloc(&self, config: &PipeConfig) -> Result<PipeId>;
    fn pipe_update(&self, pipe: PipeId, address: u8, max_packet_bytes: u16);
    fn pipe_free(&self, pipe: PipeId);
    /// On `Err` the request is not kept.
    fn submit(&self, pipe: PipeId, request: Arc<Request>) -> Result<()>;
    /// Does not wait: the request completes later, as `Canceled` or with
    /// what it got before the cancel took effect.
    fn cancel(&self, pipe: PipeId, request: &Arc<Request>);
    /// Resumes the pipe with DATA0.
    fn clear(&self, pipe: PipeId);
    /// After a disconnection.
    fn recover(&self);
    /// Whether the device is still there once the connection settled.
    async fn debounce(&self) -> bool {
        true
    }
    /// On the waiter, once `request` completed.
    fn finished(&self, _request: &Request) {}
}

#[cfg(all(not(target_os = "none"), not(test)))]
pub(crate) type Backend = crate::sim::SimHcd;
#[cfg(all(target_os = "none", CONFIG_SOC_USB_OTG_SUPPORTED))]
pub(crate) type Backend = crate::dwc2::Dwc2;
#[cfg(any(test, all(target_os = "none", not(CONFIG_SOC_USB_OTG_SUPPORTED))))]
pub(crate) type Backend = Absent;

static BACKEND: Backend = Backend::new();

pub(crate) fn backend() -> &'static Backend {
    &BACKEND
}

/// Runs `request` on `pipe`; dropping the future cancels it.
pub(crate) async fn run(pipe: PipeId, request: Arc<Request>) -> (Buffer, Result<usize>) {
    if let Err(err) = backend().submit(pipe, request.clone()) {
        return (request.take_buffer(), Err(err));
    }
    let pending = devkit::guard((), |()| backend().cancel(pipe, &request));
    let result = request.wait().await;
    core::mem::forget(pending);
    backend().finished(&request);
    (request.take_buffer(), result)
}

#[cfg(any(test, all(target_os = "none", not(CONFIG_SOC_USB_OTG_SUPPORTED))))]
pub(crate) struct Absent;

#[cfg(any(test, all(target_os = "none", not(CONFIG_SOC_USB_OTG_SUPPORTED))))]
impl Absent {
    const fn new() -> Self {
        Self
    }
}

#[cfg(any(test, all(target_os = "none", not(CONFIG_SOC_USB_OTG_SUPPORTED))))]
impl Hcd for Absent {
    fn init(&'static self, _: Sender<PortEvent>) -> Result<()> {
        Err(crate::Error::Unsupported)
    }

    async fn reset(&self) -> Result<Speed> {
        Err(crate::Error::Unsupported)
    }

    fn pipe_alloc(&self, _: &PipeConfig) -> Result<PipeId> {
        Err(crate::Error::Unsupported)
    }

    fn pipe_update(&self, _: PipeId, _: u8, _: u16) {}

    fn pipe_free(&self, _: PipeId) {}

    fn submit(&self, _: PipeId, _: Arc<Request>) -> Result<()> {
        Err(crate::Error::Unsupported)
    }

    fn cancel(&self, _: PipeId, _: &Arc<Request>) {}

    fn clear(&self, _: PipeId) {}

    fn recover(&self) {}
}
