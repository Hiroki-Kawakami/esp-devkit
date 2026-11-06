/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::{Poll, Waker};

use idf::heap::Caps;
use idf::sync::Mutex;

use crate::desc::{Configuration, DeviceDescriptor, EndpointDescriptor, TransferType};
use crate::hcd::{self, Hcd, PipeConfig, PipeId, Request, backend};
use crate::{Buffer, Error, Result, Setup, Speed};

/// One enumeration of a device; plugging it in again gives another.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DeviceId(u32);

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

pub(crate) struct DeviceInner {
    pub id: DeviceId,
    pub address: u8,
    pub speed: Speed,
    pub descriptor: DeviceDescriptor,
    /// Validated by `Configuration::parse` at enumeration.
    pub config: Vec<u8>,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    pub ep0: PipeId,
    claimed: Mutex<Vec<u8>>,
    gone: AtomicBool,
    waiters: Mutex<Vec<Waker>>,
}

impl DeviceInner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address: u8,
        speed: Speed,
        descriptor: DeviceDescriptor,
        config: Vec<u8>,
        manufacturer: String,
        product: String,
        serial: String,
        ep0: PipeId,
    ) -> Self {
        Self {
            id: DeviceId(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
            address,
            speed,
            descriptor,
            config,
            manufacturer,
            product,
            serial,
            ep0,
            claimed: Mutex::new(Vec::new()),
            gone: AtomicBool::new(false),
            waiters: Mutex::new(Vec::new()),
        }
    }

    pub fn set_gone(&self) {
        self.gone.store(true, Ordering::Release);
        for waker in core::mem::take(&mut *self.waiters.lock()) {
            waker.wake();
        }
    }
}

impl Drop for DeviceInner {
    fn drop(&mut self) {
        backend().pipe_free(self.ep0);
    }
}

/// A connected device. Every clone keeps it open; the last one dropped
/// after it is gone releases it.
#[derive(Clone)]
pub struct Device(pub(crate) Arc<DeviceInner>);

impl Device {
    pub fn id(&self) -> DeviceId {
        self.0.id
    }

    pub fn address(&self) -> u8 {
        self.0.address
    }

    pub fn speed(&self) -> Speed {
        self.0.speed
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.0.descriptor
    }

    pub fn configuration(&self) -> Configuration<'_> {
        Configuration::parse(&self.0.config).unwrap()
    }

    pub fn manufacturer(&self) -> &str {
        &self.0.manufacturer
    }

    pub fn product(&self) -> &str {
        &self.0.product
    }

    pub fn serial(&self) -> &str {
        &self.0.serial
    }

    pub fn is_gone(&self) -> bool {
        self.0.gone.load(Ordering::Acquire)
    }

    pub async fn disconnected(&self) {
        poll_fn(|cx| {
            if self.is_gone() {
                return Poll::Ready(());
            }
            let mut waiters = self.0.waiters.lock();
            if self.is_gone() {
                return Poll::Ready(());
            }
            if !waiters.iter().any(|w| w.will_wake(cx.waker())) {
                waiters.push(cx.waker().clone());
            }
            Poll::Pending
        })
        .await
    }

    /// The data stage lands in `buf`, which holds at least `setup.length`.
    pub async fn control_in(&self, setup: Setup, buf: Buffer) -> (Buffer, Result<usize>) {
        if !setup.is_in() {
            return (buf, Err(Error::InvalidArgument));
        }
        self.control(setup, buf).await
    }

    /// The data stage is the first `setup.length` bytes of `buf`.
    pub async fn control_out(&self, setup: Setup, buf: Buffer) -> (Buffer, Result<usize>) {
        if setup.is_in() {
            return (buf, Err(Error::InvalidArgument));
        }
        self.control(setup, buf).await
    }

    async fn control(&self, setup: Setup, buf: Buffer) -> (Buffer, Result<usize>) {
        if buf.len() < usize::from(setup.length) {
            return (buf, Err(Error::InvalidArgument));
        }
        if self.is_gone() {
            return (buf, Err(Error::NoDevice));
        }
        let len = usize::from(setup.length);
        hcd::run(self.0.ep0, Request::new(Some(setup), 0, buf, len, false)).await
    }

    /// Opens the alternate's bulk endpoints; sends no SET_INTERFACE. The
    /// interface is released once the `Interface` and its endpoints are
    /// dropped.
    pub fn claim(&self, interface: u8, alternate: u8) -> Result<Interface> {
        if self.is_gone() {
            return Err(Error::NoDevice);
        }
        let desc = self.configuration().interface(interface, alternate).ok_or(Error::NotFound)?;
        {
            let mut claimed = self.0.claimed.lock();
            if claimed.contains(&interface) {
                return Err(Error::Busy);
            }
            claimed.push(interface);
        }
        let mut claim = Claim { device: self.clone(), number: interface, alternate, endpoints: Vec::new() };
        for ep in desc.endpoints().filter(|ep| ep.transfer_type() == TransferType::Bulk) {
            let pipe = backend().pipe_alloc(&PipeConfig {
                kind: TransferType::Bulk,
                endpoint: ep.address,
                max_packet_bytes: ep.max_packet_bytes(),
                interval: ep.interval,
                address: self.0.address,
                speed: self.0.speed,
            })?;
            claim.endpoints.push((ep, pipe));
        }
        Ok(Interface(Arc::new(claim)))
    }
}

struct Claim {
    device: Device,
    number: u8,
    alternate: u8,
    endpoints: Vec<(EndpointDescriptor, PipeId)>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        for &(_, pipe) in &self.endpoints {
            backend().pipe_free(pipe);
        }
        self.device.0.claimed.lock().retain(|&n| n != self.number);
    }
}

#[derive(Clone)]
pub struct Interface(Arc<Claim>);

impl Interface {
    pub fn device(&self) -> &Device {
        &self.0.device
    }

    pub fn number(&self) -> u8 {
        self.0.number
    }

    pub fn alternate(&self) -> u8 {
        self.0.alternate
    }

    /// A bulk endpoint of the alternate, by address with the direction bit.
    pub fn endpoint(&self, address: u8) -> Option<Endpoint> {
        let index = self.0.endpoints.iter().position(|(ep, _)| ep.address == address)?;
        Some(Endpoint { claim: self.0.clone(), index })
    }

    pub fn endpoints(&self) -> impl Iterator<Item = Endpoint> + '_ {
        (0..self.0.endpoints.len()).map(|index| Endpoint { claim: self.0.clone(), index })
    }
}

#[derive(Clone)]
pub struct Endpoint {
    claim: Arc<Claim>,
    index: usize,
}

impl Endpoint {
    pub fn descriptor(&self) -> &EndpointDescriptor {
        &self.claim.endpoints[self.index].0
    }

    pub fn address(&self) -> u8 {
        self.descriptor().address
    }

    pub fn max_packet_bytes(&self) -> u16 {
        self.descriptor().max_packet_bytes()
    }

    fn pipe(&self) -> PipeId {
        self.claim.endpoints[self.index].1
    }

    /// Moves the first `len` bytes of `buf` out, or up to `len` bytes in. An
    /// IN transfer ends at a short packet. Transfers queued on one endpoint
    /// run in order; dropping the future cancels this one.
    pub async fn transfer(&self, buf: Buffer, len: usize) -> (Buffer, Result<usize>) {
        self.submit(buf, len, false).await
    }

    /// OUT only: ends with a zero-length packet when `len` is a multiple of
    /// the max packet size, so the device sees where it ends.
    pub async fn transfer_zlp(&self, buf: Buffer, len: usize) -> (Buffer, Result<usize>) {
        if self.descriptor().is_in() {
            return (buf, Err(Error::InvalidArgument));
        }
        self.submit(buf, len, true).await
    }

    async fn submit(&self, buf: Buffer, len: usize, zlp: bool) -> (Buffer, Result<usize>) {
        if len > buf.len() {
            return (buf, Err(Error::InvalidArgument));
        }
        if self.claim.device.is_gone() {
            return (buf, Err(Error::NoDevice));
        }
        hcd::run(self.pipe(), Request::new(None, self.address(), buf, len, zlp)).await
    }

    /// Clears the device's halt and resumes the endpoint with DATA0.
    pub async fn clear_halt(&self) -> Result<()> {
        backend().clear(self.pipe());
        let buf = Buffer::new(0, Caps::DMA | Caps::INTERNAL)?;
        self.claim.device.control_out(Setup::clear_halt(self.address()), buf).await.1.map(|_| ())
    }
}
