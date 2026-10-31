/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::c_void;
use core::mem;
use core::ptr;
use core::time::Duration;

use crate::{Error, Result, sys};

pub const FOREVER: Duration = Duration::MAX;

const DMA_DESC_NUM: u32 = 6;
const DMA_FRAME_NUM: u32 = 240;
const MCLK_BCLK_DIV: u32 = 8;

fn timeout_ms(timeout: Duration) -> u32 {
    if timeout == FOREVER {
        return u32::MAX;
    }
    timeout.as_nanos().div_ceil(1_000_000).min(u128::from(u32::MAX)) as u32
}

/// -1 leaves a pin unconnected.
#[derive(Clone, Copy, Debug)]
pub struct Pins {
    pub mclk: i32,
    pub bclk: i32,
    pub ws: i32,
    pub dout: i32,
    pub din: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StdFormat {
    pub sample_rate: u32,
    /// 16, 24 or 32.
    pub bits: u32,
    /// 1 or 2.
    pub channels: u8,
}

impl StdFormat {
    fn clock(&self) -> sys::i2s_std_clk_config_t {
        let mut cfg: sys::i2s_std_clk_config_t = unsafe { mem::zeroed() };
        cfg.sample_rate_hz = self.sample_rate;
        cfg.clk_src = sys::soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT;
        cfg.mclk_multiple = sys::i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256;
        cfg.bclk_div = MCLK_BCLK_DIV;
        cfg
    }

    fn philips_slot(&self) -> sys::i2s_std_slot_config_t {
        let mut cfg: sys::i2s_std_slot_config_t = unsafe { mem::zeroed() };
        cfg.data_bit_width = self.bits as _;
        cfg.slot_bit_width = sys::i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO;
        cfg.slot_mode = if self.channels == 1 {
            sys::i2s_slot_mode_t_I2S_SLOT_MODE_MONO
        } else {
            sys::i2s_slot_mode_t_I2S_SLOT_MODE_STEREO
        };
        cfg.slot_mask = sys::i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH;
        cfg.ws_width = self.bits;
        cfg.bit_shift = true;
        #[cfg(CONFIG_SOC_I2S_HW_VERSION_1)]
        {
            cfg.msb_right = self.bits <= 16;
        }
        #[cfg(not(CONFIG_SOC_I2S_HW_VERSION_1))]
        {
            cfg.left_align = true;
        }
        cfg
    }
}

/// Runs in an ISR. `size` of the event is the buffer sent or dropped.
pub type Callback =
    unsafe extern "C" fn(sys::i2s_chan_handle_t, *mut sys::i2s_event_data_t, *mut c_void) -> bool;

pub struct Callbacks {
    pub on_sent: Option<Callback>,
    pub on_send_q_ovf: Option<Callback>,
}

/// A master transmit channel in standard mode.
pub struct TxChannel {
    handle: sys::i2s_chan_handle_t,
}

unsafe impl Send for TxChannel {}
unsafe impl Sync for TxChannel {}

impl TxChannel {
    /// A channel that sends zeros once the writer runs out of data.
    pub fn new(port: i32, format: StdFormat, pins: Pins) -> Result<Self> {
        let mut cfg: sys::i2s_chan_config_t = unsafe { mem::zeroed() };
        cfg.id = port;
        cfg.role = sys::i2s_role_t_I2S_ROLE_MASTER;
        cfg.dma_desc_num = DMA_DESC_NUM;
        cfg.dma_frame_num = DMA_FRAME_NUM;
        cfg.__bindgen_anon_1.auto_clear = true;
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::i2s_new_channel(&cfg, &mut handle, ptr::null_mut()) })?;
        let channel = Self { handle };

        let mut std_cfg: sys::i2s_std_config_t = unsafe { mem::zeroed() };
        std_cfg.clk_cfg = format.clock();
        std_cfg.slot_cfg = format.philips_slot();
        std_cfg.gpio_cfg.mclk = pins.mclk;
        std_cfg.gpio_cfg.bclk = pins.bclk;
        std_cfg.gpio_cfg.ws = pins.ws;
        std_cfg.gpio_cfg.dout = pins.dout;
        std_cfg.gpio_cfg.din = pins.din;
        Error::check(unsafe { sys::i2s_channel_init_std_mode(channel.handle, &std_cfg) })?;
        Ok(channel)
    }

    /// `context` must outlive the channel.
    pub unsafe fn set_callbacks(&self, callbacks: &Callbacks, context: *mut c_void) -> Result<()> {
        let mut cbs: sys::i2s_event_callbacks_t = unsafe { mem::zeroed() };
        cbs.on_sent = callbacks.on_sent;
        cbs.on_send_q_ovf = callbacks.on_send_q_ovf;
        Error::check(unsafe { sys::i2s_channel_register_event_callback(self.handle, &cbs, context) })
    }

    /// The channel must be disabled.
    pub fn reconfigure(&self, format: StdFormat) -> Result<()> {
        let clock = format.clock();
        Error::check(unsafe { sys::i2s_channel_reconfig_std_clock(self.handle, &clock) })?;
        let slot = format.philips_slot();
        Error::check(unsafe { sys::i2s_channel_reconfig_std_slot(self.handle, &slot) })
    }

    pub fn enable(&self) -> Result<()> {
        Error::check(unsafe { sys::i2s_channel_enable(self.handle) })
    }

    pub fn disable(&self) -> Result<()> {
        Error::check(unsafe { sys::i2s_channel_disable(self.handle) })
    }

    /// Blocks while the DMA ring is full. Returns the bytes accepted, which a
    /// failure leaves short of `data`.
    pub fn write(&self, data: &[u8], timeout: Duration) -> (usize, Result<()>) {
        let mut written = 0;
        let code = unsafe {
            sys::i2s_channel_write(
                self.handle,
                data.as_ptr().cast(),
                data.len(),
                &mut written,
                timeout_ms(timeout),
            )
        };
        (written, Error::check(code))
    }

    /// Total bytes of the DMA ring.
    pub fn ring_bytes(&self) -> Result<usize> {
        let mut info: sys::i2s_chan_info_t = unsafe { mem::zeroed() };
        Error::check(unsafe { sys::i2s_channel_get_info(self.handle, &mut info) })?;
        Ok(info.total_dma_buf_size as usize)
    }
}

impl Drop for TxChannel {
    fn drop(&mut self) {
        unsafe { sys::i2s_del_channel(self.handle) };
    }
}
