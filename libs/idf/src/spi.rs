/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::mem;

use crate::{Error, Result, sys};

pub struct BusConfig {
    pub host: i32,
    pub sclk: i32,
    pub mosi: i32,
    pub miso: Option<i32>,
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
