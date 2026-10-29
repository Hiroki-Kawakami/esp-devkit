/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::c_int;
use core::marker::PhantomData;
use core::mem;
use core::ptr;
use core::time::Duration;

use crate::{Error, Result, sys};

pub const FOREVER: Duration = Duration::MAX;

fn timeout_ms(timeout: Duration) -> c_int {
    if timeout == FOREVER {
        return -1;
    }
    timeout.as_nanos().div_ceil(1_000_000).min(c_int::MAX as u128) as c_int
}

pub struct BusConfig {
    pub port: i32,
    pub sda: i32,
    pub scl: i32,
    pub glitch_ignore_cnt: u8,
    pub internal_pullup: bool,
}

pub struct MasterBus(sys::i2c_master_bus_handle_t);

unsafe impl Send for MasterBus {}
unsafe impl Sync for MasterBus {}

impl MasterBus {
    pub fn new(config: &BusConfig) -> Result<Self> {
        let mut cfg: sys::i2c_master_bus_config_t = unsafe { mem::zeroed() };
        cfg.i2c_port = config.port as _;
        cfg.sda_io_num = config.sda as _;
        cfg.scl_io_num = config.scl as _;
        #[cfg(target_os = "none")]
        {
            cfg.__bindgen_anon_1.clk_source = sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
        }
        cfg.glitch_ignore_cnt = config.glitch_ignore_cnt;
        cfg.flags.set_enable_internal_pullup(config.internal_pullup as u32);
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::i2c_new_master_bus(&cfg, &mut handle) })?;
        Ok(Self(handle))
    }

    pub fn probe(&self, address: u16, timeout: Duration) -> Result<()> {
        Error::check(unsafe { sys::i2c_master_probe(self.0, address, timeout_ms(timeout)) })
    }

    pub fn add_device(&self, address: u16, scl_speed_hz: u32) -> Result<Device<'_>> {
        let mut cfg: sys::i2c_device_config_t = unsafe { mem::zeroed() };
        cfg.dev_addr_length = sys::i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7;
        cfg.device_address = address;
        cfg.scl_speed_hz = scl_speed_hz;
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::i2c_master_bus_add_device(self.0, &cfg, &mut handle) })?;
        Ok(Device { handle, _bus: PhantomData })
    }
}

impl Drop for MasterBus {
    fn drop(&mut self) {
        unsafe { sys::i2c_del_master_bus(self.0) };
    }
}

pub struct Device<'bus> {
    handle: sys::i2c_master_dev_handle_t,
    _bus: PhantomData<&'bus MasterBus>,
}

unsafe impl Send for Device<'_> {}
unsafe impl Sync for Device<'_> {}

impl Device<'_> {
    pub fn transmit(&self, data: &[u8], timeout: Duration) -> Result<()> {
        Error::check(unsafe {
            sys::i2c_master_transmit(self.handle, data.as_ptr(), data.len(), timeout_ms(timeout))
        })
    }

    pub fn receive(&self, buf: &mut [u8], timeout: Duration) -> Result<()> {
        Error::check(unsafe {
            sys::i2c_master_receive(self.handle, buf.as_mut_ptr(), buf.len(), timeout_ms(timeout))
        })
    }

    pub fn transmit_receive(&self, data: &[u8], buf: &mut [u8], timeout: Duration) -> Result<()> {
        Error::check(unsafe {
            sys::i2c_master_transmit_receive(
                self.handle,
                data.as_ptr(),
                data.len(),
                buf.as_mut_ptr(),
                buf.len(),
                timeout_ms(timeout),
            )
        })
    }
}

impl Drop for Device<'_> {
    fn drop(&mut self) {
        unsafe { sys::i2c_master_bus_rm_device(self.handle) };
    }
}
