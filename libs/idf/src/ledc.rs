/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::mem;

use crate::{Error, Result, sys};

const SPEED_MODE: sys::ledc_mode_t = sys::ledc_mode_t_LEDC_LOW_SPEED_MODE;

pub struct TimerConfig {
    pub timer: u32,
    pub freq_hz: u32,
    pub resolution_bits: u32,
}

pub fn configure_timer(config: &TimerConfig) -> Result<()> {
    let mut cfg: sys::ledc_timer_config_t = unsafe { mem::zeroed() };
    cfg.speed_mode = SPEED_MODE;
    cfg.duty_resolution = config.resolution_bits as _;
    cfg.timer_num = config.timer as _;
    cfg.freq_hz = config.freq_hz;
    cfg.clk_cfg = sys::soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK;
    Error::check(unsafe { sys::ledc_timer_config(&cfg) })
}

pub struct ChannelConfig {
    pub channel: u32,
    pub timer: u32,
    pub gpio: i32,
    pub duty: u32,
}

pub struct Channel(sys::ledc_channel_t);

impl Channel {
    pub fn new(config: &ChannelConfig) -> Result<Self> {
        let mut cfg: sys::ledc_channel_config_t = unsafe { mem::zeroed() };
        cfg.gpio_num = config.gpio;
        cfg.speed_mode = SPEED_MODE;
        cfg.channel = config.channel as _;
        cfg.timer_sel = config.timer as _;
        cfg.duty = config.duty;
        Error::check(unsafe { sys::ledc_channel_config(&cfg) })?;
        Ok(Self(config.channel as _))
    }

    pub fn set_duty(&self, duty: u32) -> Result<()> {
        Error::check(unsafe { sys::ledc_set_duty(SPEED_MODE, self.0, duty) })?;
        Error::check(unsafe { sys::ledc_update_duty(SPEED_MODE, self.0) })
    }

    pub fn duty(&self) -> u32 {
        unsafe { sys::ledc_get_duty(SPEED_MODE, self.0) }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        unsafe { sys::ledc_stop(SPEED_MODE, self.0, 0) };
    }
}
