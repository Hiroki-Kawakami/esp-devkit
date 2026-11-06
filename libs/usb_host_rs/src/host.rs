/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::time::Duration;

use idf::executor::{self, Receiver};
use idf::heap::Caps;
use idf::log::{error, info, warn};
use idf::thread;

use crate::desc::{self, Configuration, DeviceDescriptor, TransferType};
use crate::device::DeviceInner;
use crate::hcd::{self, Hcd, PipeConfig, PipeId, PortEvent, Request, backend};
use crate::watch;
use crate::{Buffer, Device, Error, Result, Setup, Speed};

const TAG: &CStr = c"usb_host";

const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
const SET_ADDRESS_RECOVERY: Duration = Duration::from_millis(10);
const RETRY_DELAY: Duration = Duration::from_millis(100);
const ENUM_BUFFER_BYTES: usize = 4096;
const ENUM_ATTEMPTS: usize = 3;
const STRING_BYTES: u16 = 255;

/// The host task: enumeration runs on it.
pub struct Config {
    pub priority: u32,
    pub core: Option<u32>,
    pub stack_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { priority: 5, core: None, stack_size: 8192 }
    }
}

static STARTED: AtomicBool = AtomicBool::new(false);
static NEXT_ADDRESS: AtomicU8 = AtomicU8::new(1);

/// Once. The port's VBUS is the caller's.
pub fn start(config: &Config) -> Result<()> {
    if STARTED.swap(true, Ordering::AcqRel) {
        return Err(Error::AlreadyStarted);
    }
    let (tx, rx) = executor::channel(4);
    if let Err(err) = backend().init(tx) {
        STARTED.store(false, Ordering::Release);
        return Err(err);
    }
    thread::Builder::new()
        .name("usbh")
        .stack_size(config.stack_size)
        .priority(config.priority)
        .core(config.core)
        .spawn_local_executor(move |local| drop(local.spawn_local(run(rx))))
        .map_err(|err| {
            error!(TAG, "host task: {err}");
            Error::NoMemory
        })?;
    Ok(())
}

async fn run(mut events: Receiver<PortEvent>) {
    let mut current: Option<Device> = None;
    loop {
        let event = events.recv().await;
        if let Some(device) = current.take() {
            info!(TAG, "device at {} gone", device.address());
            device.0.set_gone();
            backend().recover();
            watch::left(device);
        }
        if event == PortEvent::Disconnected || !backend().debounce().await {
            continue;
        }
        let device = match enumerate().await {
            Ok(device) => device,
            Err(err) => {
                error!(TAG, "enumeration failed: {err}");
                continue;
            }
        };
        let d = device.descriptor();
        info!(
            TAG,
            "device at {}: {:04x}:{:04x} \"{}\", {} speed",
            device.address(),
            d.vendor_id,
            d.product_id,
            device.product(),
            match device.speed() {
                Speed::High => "high",
                Speed::Full => "full",
                Speed::Low => "low",
            }
        );
        current = Some(device.clone());
        watch::arrived(device);
    }
}

async fn enumerate() -> Result<Device> {
    let mut last = Error::Failed;
    for attempt in 0..ENUM_ATTEMPTS {
        if attempt > 0 {
            executor::sleep(RETRY_DELAY).await;
        }
        match enumerate_once(attempt == 1).await {
            Ok(device) => return Ok(device),
            Err(Error::NoDevice) => return Err(Error::NoDevice),
            Err(err) => last = err,
        }
    }
    Err(last)
}

async fn control(pipe: PipeId, setup: Setup, buf: Buffer, stage: &str) -> Result<(Buffer, usize)> {
    let len = usize::from(setup.length);
    let request = Request::new(Some(setup), 0, buf, len, false);
    let result = match executor::timeout(CONTROL_TIMEOUT, hcd::run(pipe, request)).await {
        Ok((buf, Ok(n))) => Ok((buf, n)),
        Ok((_, Err(err))) => Err(err),
        Err(_) => Err(Error::TimedOut),
    };
    if let Err(err) = result {
        warn!(TAG, "enumeration: {stage}: {err}");
    }
    result
}

async fn get_descriptor(pipe: PipeId, buf: Buffer, kind: u8, index: u8, language: u16, length: u16, stage: &str) -> Result<(Buffer, usize)> {
    control(pipe, Setup::get_descriptor(kind, index, language, length), buf, stage).await
}

/* espressif/usb resets the bus a second time between the first descriptor
   read and SET_ADDRESS. Some devices then never answer at their new address,
   so the first attempt skips it; the retry keeps it for devices that need it. */
async fn enumerate_once(second_reset: bool) -> Result<Device> {
    let hcd = backend();
    let speed = hcd.reset().await?;
    let ep0 = hcd.pipe_alloc(&PipeConfig {
        kind: TransferType::Control,
        endpoint: 0,
        max_packet_bytes: if speed == Speed::Low { 8 } else { 64 },
        interval: 0,
        address: 0,
        speed,
    })?;
    let ep0 = devkit::guard(ep0, |pipe| backend().pipe_free(pipe));
    let buf = Buffer::new(ENUM_BUFFER_BYTES, Caps::DMA | Caps::INTERNAL)?;

    let (buf, n) = get_descriptor(*ep0, buf, desc::DEVICE, 0, 0, 8, "device descriptor (8)").await?;
    if n < 8 || buf[1] != desc::DEVICE {
        return Err(Error::InvalidResponse);
    }
    let mps0 = u16::from(buf[7]);
    if second_reset {
        hcd.reset().await?;
    }
    hcd.pipe_update(*ep0, 0, mps0);
    let address = NEXT_ADDRESS.load(Ordering::Relaxed);
    NEXT_ADDRESS.store(if address >= 127 { 1 } else { address + 1 }, Ordering::Relaxed);
    let (buf, _) = control(*ep0, Setup::set_address(address), buf, "SET_ADDRESS").await?;
    executor::sleep(SET_ADDRESS_RECOVERY).await;
    hcd.pipe_update(*ep0, address, mps0);

    let (buf, n) =
        get_descriptor(*ep0, buf, desc::DEVICE, 0, 0, desc::DEVICE_BYTES as u16, "device descriptor").await?;
    let descriptor = DeviceDescriptor::parse(&buf[..n]).ok_or(Error::InvalidResponse)?;

    let (buf, n) = get_descriptor(*ep0, buf, desc::CONFIGURATION, 0, 0, 9, "config descriptor (9)").await?;
    if n < 9 {
        return Err(Error::InvalidResponse);
    }
    let total = u16::from_le_bytes([buf[2], buf[3]]);
    if usize::from(total) < desc::CONFIGURATION_BYTES || usize::from(total) > ENUM_BUFFER_BYTES {
        warn!(TAG, "enumeration: config descriptor size {total}");
        return Err(Error::InvalidResponse);
    }
    let (buf, n) = get_descriptor(*ep0, buf, desc::CONFIGURATION, 0, 0, total, "config descriptor").await?;
    if n < usize::from(total) || Configuration::parse(&buf[..n]).is_none() {
        return Err(Error::InvalidResponse);
    }
    let config = buf[..n].to_vec();
    let value = Configuration::parse(&config).unwrap().value();

    let [manufacturer, product, serial] = read_strings(*ep0, &descriptor).await;
    control(*ep0, Setup::set_configuration(value), buf, "SET_CONFIGURATION").await?;

    let inner = DeviceInner::new(address, speed, descriptor, config, manufacturer, product, serial, *ep0);
    devkit::Guard::into_inner(ep0);
    Ok(Device(Arc::new(inner)))
}

/// Manufacturer, product and serial; empty where the device has none or
/// does not answer.
async fn read_strings(pipe: PipeId, d: &DeviceDescriptor) -> [String; 3] {
    let mut out = [String::new(), String::new(), String::new()];
    let indexes = [d.manufacturer, d.product, d.serial];
    if indexes.iter().all(|&i| i == 0) {
        return out;
    }
    let read = |index: u8, language: u16| async move {
        let buf = Buffer::new(usize::from(STRING_BYTES), Caps::DMA | Caps::INTERNAL).ok()?;
        let (buf, n) = get_descriptor(pipe, buf, desc::STRING, index, language, STRING_BYTES, "string").await.ok()?;
        Some((buf, n))
    };
    let Some((buf, n)) = read(0, 0).await else { return out };
    if n < 4 {
        return out;
    }
    let language = u16::from_le_bytes([buf[2], buf[3]]);
    for (slot, index) in out.iter_mut().zip(indexes) {
        if index != 0
            && let Some((buf, n)) = read(index, language).await
        {
            *slot = desc::string(&buf[..n]);
        }
    }
    out
}
