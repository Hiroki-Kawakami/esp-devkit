/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::{mem, ptr};

use crate::{Error, Result, sys};

pub struct BusConfig {
    pub host: i32,
    pub sclk: i32,
    /// Data 0 on a quad bus.
    pub mosi: i32,
    /// Data 1 on a quad bus.
    pub miso: Option<i32>,
    /// Data 2 and 3, for quad transfers.
    pub quad: Option<[i32; 2]>,
    /// The largest single transfer; longer ones are split by the devices.
    pub max_transfer_bytes: usize,
}

struct BusHandle(sys::spi_host_device_t);

impl Drop for BusHandle {
    fn drop(&mut self) {
        unsafe { sys::spi_bus_free(self.0) };
    }
}

/// A master bus with DMA; it is freed after the last device on it.
#[derive(Clone)]
pub struct Bus(Arc<BusHandle>);

impl Bus {
    pub fn new(config: &BusConfig) -> Result<Self> {
        let mut cfg: sys::spi_bus_config_t = unsafe { mem::zeroed() };
        let pins = unsafe { &mut cfg.__bindgen_anon_1.iocfg };
        pins.fill(-1);
        pins[0] = config.mosi;
        pins[1] = config.miso.unwrap_or(-1);
        pins[2] = config.sclk;
        if let Some([data2, data3]) = config.quad {
            pins[3] = data2;
            pins[4] = data3;
        }
        cfg.max_transfer_sz = config.max_transfer_bytes as _;
        let host = config.host as sys::spi_host_device_t;
        Error::check(unsafe { sys::spi_bus_initialize(host, &cfg, sys::spi_common_dma_t_SPI_DMA_CH_AUTO) })?;
        Ok(Self(Arc::new(BusHandle(host))))
    }

    pub fn host(&self) -> i32 {
        self.0.0 as i32
    }

    pub(crate) fn keep(&self) -> Arc<dyn core::any::Any + Send + Sync> {
        self.0.clone()
    }
}

pub struct DeviceConfig {
    pub clock_hz: u32,
    pub mode: u8,
    /// `None` leaves the chip select to the caller.
    pub cs: Option<i32>,
    /// How late the device's output reaches the pin after the clock edge.
    pub input_delay_ns: i32,
    /// Sends and receives in separate transactions, which lets reads at
    /// higher clocks be compensated for `input_delay_ns`.
    pub half_duplex: bool,
    pub queue_size: usize,
}

/// A transaction `Device::queue_transmit` sends; it must not move or drop
/// until `Device::finish` returns it.
pub struct Transfer(sys::spi_transaction_t);

unsafe impl Send for Transfer {}

impl Default for Transfer {
    fn default() -> Self {
        Self(unsafe { mem::zeroed() })
    }
}

/// Holds the bus for one device; the others wait until it drops.
pub struct Acquired<'a>(&'a Device);

impl Drop for Acquired<'_> {
    fn drop(&mut self) {
        unsafe { sys::spi_device_release_bus(self.0.handle) };
    }
}

pub struct Device {
    handle: sys::spi_device_handle_t,
    _bus: Bus,
}

unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { sys::spi_bus_remove_device(self.handle) };
    }
}

impl Device {
    pub fn new(bus: &Bus, config: &DeviceConfig) -> Result<Self> {
        let mut cfg: sys::spi_device_interface_config_t = unsafe { mem::zeroed() };
        cfg.clock_speed_hz = config.clock_hz as i32;
        cfg.mode = config.mode;
        cfg.spics_io_num = config.cs.unwrap_or(-1);
        cfg.input_delay_ns = config.input_delay_ns;
        cfg.queue_size = config.queue_size as i32;
        if config.half_duplex {
            cfg.flags = sys::SPI_DEVICE_HALFDUPLEX;
        }
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::spi_bus_add_device(bus.host() as sys::spi_host_device_t, &cfg, &mut handle) })?;
        Ok(Self { handle, _bus: bus.clone() })
    }

    pub fn acquire(&self) -> Result<Acquired<'_>> {
        Error::check(unsafe { sys::spi_device_acquire_bus(self.handle, u32::MAX) })?;
        Ok(Acquired(self))
    }

    /// Spins until it is sent, for a few bytes; up to four are copied.
    pub fn transmit_polling(&self, tx: &[u8]) -> Result<()> {
        let mut trans: sys::spi_transaction_t = unsafe { mem::zeroed() };
        trans.length = tx.len() * 8;
        if tx.len() <= 4 {
            trans.flags = sys::SPI_TRANS_USE_TXDATA;
            unsafe { trans.__bindgen_anon_1.tx_data[..tx.len()].copy_from_slice(tx) };
        } else {
            trans.__bindgen_anon_1.tx_buffer = tx.as_ptr().cast();
        }
        Error::check(unsafe { sys::spi_device_polling_transmit(self.handle, &mut trans) })
    }

    /// Spins until `rx` is filled. Up to four bytes need no DMA-capable
    /// buffer; past that, the driver allocates one when `rx` is not.
    pub fn receive_polling(&self, rx: &mut [u8]) -> Result<()> {
        let mut trans: sys::spi_transaction_t = unsafe { mem::zeroed() };
        trans.rxlength = rx.len() * 8;
        if rx.len() <= 4 {
            trans.flags = sys::SPI_TRANS_USE_RXDATA;
        } else {
            trans.__bindgen_anon_2.rx_buffer = rx.as_mut_ptr().cast();
        }
        Error::check(unsafe { sys::spi_device_polling_transmit(self.handle, &mut trans) })?;
        if rx.len() <= 4 {
            rx.copy_from_slice(unsafe { &trans.__bindgen_anon_2.rx_data[..rx.len()] });
        }
        Ok(())
    }

    /// Queues `tx`, which DMA must be able to read, behind earlier transfers.
    /// `transfer` and `tx` must stay put and unchanged until `finish` hands
    /// the transfer back.
    pub unsafe fn queue_transmit(&self, transfer: &mut Transfer, tx: &[u8]) -> Result<()> {
        transfer.0 = unsafe { mem::zeroed() };
        transfer.0.length = tx.len() * 8;
        transfer.0.__bindgen_anon_1.tx_buffer = tx.as_ptr().cast();
        Error::check(unsafe { sys::spi_device_queue_trans(self.handle, &mut transfer.0, u32::MAX) })
    }

    /// Blocks until the oldest queued transfer is done.
    pub fn finish(&self) -> Result<()> {
        let mut done = ptr::null_mut();
        Error::check(unsafe { sys::spi_device_get_trans_result(self.handle, &mut done, u32::MAX) })
    }
}
