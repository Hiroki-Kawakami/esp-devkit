/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

use crate::{Error, Result, sys};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Atten {
    Db0,
    Db2_5,
    Db6,
    Db12,
}

impl Atten {
    fn raw(self) -> sys::adc_atten_t {
        match self {
            Self::Db0 => sys::adc_atten_t_ADC_ATTEN_DB_0,
            Self::Db2_5 => sys::adc_atten_t_ADC_ATTEN_DB_2_5,
            Self::Db6 => sys::adc_atten_t_ADC_ATTEN_DB_6,
            Self::Db12 => sys::adc_atten_t_ADC_ATTEN_DB_12,
        }
    }
}

/// A GPIO read one sample at a time through its ADC unit, which it takes for
/// itself, and calibrated to millivolts.
pub struct Input {
    unit: sys::adc_oneshot_unit_handle_t,
    channel: sys::adc_channel_t,
    cali: sys::adc_cali_handle_t,
}

unsafe impl Send for Input {}
unsafe impl Sync for Input {}

impl Input {
    /// `NOT_SUPPORTED` when the chip cannot calibrate the channel.
    pub fn new(gpio: i32, atten: Atten) -> Result<Self> {
        let (mut unit_id, mut channel) = (0, 0);
        Error::check(unsafe { sys::adc_oneshot_io_to_channel(gpio, &mut unit_id, &mut channel) })?;
        let mut config: sys::adc_oneshot_unit_init_cfg_t = unsafe { core::mem::zeroed() };
        config.unit_id = unit_id;
        let mut unit = ptr::null_mut();
        Error::check(unsafe { sys::adc_oneshot_new_unit(&config, &mut unit) })?;
        let mut input = Self { unit, channel, cali: ptr::null_mut() };
        let chan = sys::adc_oneshot_chan_cfg_t { atten: atten.raw(), bitwidth: sys::adc_bitwidth_t_ADC_BITWIDTH_DEFAULT };
        Error::check(unsafe { sys::adc_oneshot_config_channel(unit, channel, &chan) })?;
        Error::check(unsafe { sys::idf_adc_cali_create(unit_id, channel, atten.raw(), &mut input.cali) })?;
        Ok(input)
    }

    pub fn read_raw(&self) -> Result<i32> {
        let mut raw = 0;
        Error::check(unsafe { sys::adc_oneshot_read(self.unit, self.channel, &mut raw) })?;
        Ok(raw)
    }

    /// The mean of `samples` reads.
    pub fn read_mv(&self, samples: u8) -> Result<u32> {
        let samples = samples.max(1);
        let mut sum = 0;
        for _ in 0..samples {
            sum += self.read_raw()?;
        }
        let mut mv = 0;
        Error::check(unsafe { sys::adc_cali_raw_to_voltage(self.cali, sum / i32::from(samples), &mut mv) })?;
        Ok(mv.max(0) as u32)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if !self.cali.is_null() {
            unsafe { sys::idf_adc_cali_delete(self.cali) };
        }
        unsafe { sys::adc_oneshot_del_unit(self.unit) };
    }
}
