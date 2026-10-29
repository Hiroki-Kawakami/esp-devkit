/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

// SOC_* capabilities have no prompt and are absent from sdkconfig, and so
// unknown to --check-cfg, on chips without them.
#![allow(unexpected_cfgs)]

use alloc::sync::Arc;
use core::any::Any;
use core::ffi::c_void;
use core::ptr;

use crate::{Error, Result, sys};

pub struct PanelIo {
    handle: sys::esp_lcd_panel_io_handle_t,
    _parent: Option<Arc<dyn Any + Send + Sync>>,
}

unsafe impl Send for PanelIo {}
unsafe impl Sync for PanelIo {}

impl PanelIo {
    pub fn tx_param(&self, cmd: i32, param: &[u8]) -> Result<()> {
        let data = if param.is_empty() { ptr::null() } else { param.as_ptr().cast() };
        Error::check(unsafe { sys::esp_lcd_panel_io_tx_param(self.handle, cmd, data, param.len()) })
    }
}

impl Drop for PanelIo {
    fn drop(&mut self) {
        unsafe { sys::esp_lcd_panel_io_del(self.handle) };
    }
}

pub struct Panel {
    handle: sys::esp_lcd_panel_handle_t,
    _parent: Option<Arc<dyn Any + Send + Sync>>,
}

unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

impl Panel {
    pub fn init(&self) -> Result<()> {
        Error::check(unsafe { sys::esp_lcd_panel_init(self.handle) })
    }

    /// `data` must hold the `(x_end - x_start) * (y_end - y_start)` pixels in
    /// the panel's input format.
    pub unsafe fn draw_bitmap(&self, x_start: i32, y_start: i32, x_end: i32, y_end: i32,
                              data: *const c_void) -> Result<()> {
        Error::check(unsafe {
            sys::esp_lcd_panel_draw_bitmap(self.handle, x_start, y_start, x_end, y_end, data)
        })
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe { sys::esp_lcd_panel_del(self.handle) };
    }
}

#[cfg(CONFIG_SOC_MIPI_DSI_SUPPORTED)]
pub mod dsi {
    use alloc::sync::Arc;
    use core::mem;
    use core::ptr::{self, NonNull};

    use super::{Panel, PanelIo};
    use crate::{Error, Result, sys};

    pub const MAX_FRAME_BUFFERS: usize = 3;

    // MIPI_DSI_PHY_PLLREF_CLK_SRC_DEFAULT (XTAL) exists only from chip v3.0; the
    // HAL aborts on it on earlier revisions.
    #[cfg(CONFIG_ESP32P4_SELECTS_REV_LESS_V3)]
    const PHY_CLK_SRC_DEFAULT: sys::soc_periph_mipi_dsi_phy_pllref_clk_src_t =
        sys::soc_periph_mipi_dsi_phy_pllref_clk_src_t_MIPI_DSI_PHY_PLLREF_CLK_SRC_DEFAULT_LEGACY;
    #[cfg(not(CONFIG_ESP32P4_SELECTS_REV_LESS_V3))]
    const PHY_CLK_SRC_DEFAULT: sys::soc_periph_mipi_dsi_phy_pllref_clk_src_t =
        sys::soc_periph_mipi_dsi_phy_pllref_clk_src_t_MIPI_DSI_PHY_PLLREF_CLK_SRC_DEFAULT;

    struct BusHandle(sys::esp_lcd_dsi_bus_handle_t);

    unsafe impl Send for BusHandle {}
    unsafe impl Sync for BusHandle {}

    impl Drop for BusHandle {
        fn drop(&mut self) {
            unsafe { sys::esp_lcd_del_dsi_bus(self.0) };
        }
    }

    pub struct BusConfig {
        pub bus_id: i32,
        pub num_data_lanes: u8,
        pub lane_bit_rate_mbps: f32,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum ColorFormat {
        Rgb565,
        Rgb888,
    }

    impl ColorFormat {
        fn raw(self) -> sys::lcd_color_format_t {
            match self {
                Self::Rgb565 => sys::lcd_color_format_t_LCD_COLOR_FMT_RGB565,
                Self::Rgb888 => sys::lcd_color_format_t_LCD_COLOR_FMT_RGB888,
            }
        }
    }

    #[derive(Clone, Copy, Default)]
    pub struct VideoTiming {
        pub h_size: u32,
        pub v_size: u32,
        pub hsync_pulse_width: u32,
        pub hsync_back_porch: u32,
        pub hsync_front_porch: u32,
        pub vsync_pulse_width: u32,
        pub vsync_back_porch: u32,
        pub vsync_front_porch: u32,
    }

    pub struct DpiPanelConfig {
        pub virtual_channel: u8,
        pub dpi_clock_freq_mhz: f32,
        pub in_color_format: ColorFormat,
        pub out_color_format: ColorFormat,
        pub num_fbs: u8,
        pub video_timing: VideoTiming,
    }

    pub struct Bus(Arc<BusHandle>);

    impl Bus {
        pub fn new(config: &BusConfig) -> Result<Self> {
            let mut cfg: sys::esp_lcd_dsi_bus_config_t = unsafe { mem::zeroed() };
            cfg.bus_id = config.bus_id;
            cfg.num_data_lanes = config.num_data_lanes;
            cfg.phy_clk_src = PHY_CLK_SRC_DEFAULT;
            cfg.lane_bit_rate_mbps = config.lane_bit_rate_mbps;
            let mut handle = ptr::null_mut();
            Error::check(unsafe { sys::esp_lcd_new_dsi_bus(&cfg, &mut handle) })?;
            Ok(Self(Arc::new(BusHandle(handle))))
        }

        pub fn new_dbi_io(&self, virtual_channel: u8, cmd_bits: i32, param_bits: i32) -> Result<PanelIo> {
            let cfg = sys::esp_lcd_dbi_io_config_t {
                virtual_channel,
                lcd_cmd_bits: cmd_bits,
                lcd_param_bits: param_bits,
            };
            let mut handle = ptr::null_mut();
            Error::check(unsafe { sys::esp_lcd_new_panel_io_dbi(self.0.0, &cfg, &mut handle) })?;
            Ok(PanelIo { handle, _parent: Some(self.0.clone()) })
        }

        pub fn new_dpi_panel(&self, config: &DpiPanelConfig) -> Result<DpiPanel> {
            let t = &config.video_timing;
            let mut cfg: sys::esp_lcd_dpi_panel_config_t = unsafe { mem::zeroed() };
            cfg.virtual_channel = config.virtual_channel;
            cfg.dpi_clk_src = sys::soc_periph_mipi_dsi_dpi_clk_src_t_MIPI_DSI_DPI_CLK_SRC_DEFAULT;
            cfg.dpi_clock_freq_mhz = config.dpi_clock_freq_mhz;
            cfg.in_color_format = config.in_color_format.raw();
            cfg.out_color_format = config.out_color_format.raw();
            cfg.num_fbs = config.num_fbs;
            cfg.video_timing = sys::esp_lcd_video_timing_t {
                h_size: t.h_size,
                v_size: t.v_size,
                hsync_pulse_width: t.hsync_pulse_width,
                hsync_back_porch: t.hsync_back_porch,
                hsync_front_porch: t.hsync_front_porch,
                vsync_pulse_width: t.vsync_pulse_width,
                vsync_back_porch: t.vsync_back_porch,
                vsync_front_porch: t.vsync_front_porch,
            };
            let mut handle = ptr::null_mut();
            Error::check(unsafe { sys::esp_lcd_new_panel_dpi(self.0.0, &cfg, &mut handle) })?;
            Ok(DpiPanel {
                panel: Panel { handle, _parent: Some(self.0.clone()) },
                num_fbs: config.num_fbs.max(1),
                dma2d: false,
            })
        }
    }

    pub struct DpiPanel {
        panel: Panel,
        num_fbs: u8,
        dma2d: bool,
    }

    impl DpiPanel {
        pub fn panel(&self) -> &Panel {
            &self.panel
        }

        pub fn enable_dma2d(&mut self) -> Result<()> {
            Error::check(unsafe { sys::esp_lcd_dpi_panel_enable_dma2d(self.panel.handle) })?;
            self.dma2d = true;
            Ok(())
        }

        pub fn frame_buffers(&self) -> Result<[Option<NonNull<u8>>; MAX_FRAME_BUFFERS]> {
            let mut fb: [*mut core::ffi::c_void; MAX_FRAME_BUFFERS] = [ptr::null_mut(); MAX_FRAME_BUFFERS];
            let [fb0, fb1, fb2] = &mut fb;
            Error::check(unsafe {
                sys::esp_lcd_dpi_panel_get_frame_buffer(
                    self.panel.handle,
                    u32::from(self.num_fbs),
                    fb0 as *mut *mut core::ffi::c_void,
                    fb1 as *mut *mut core::ffi::c_void,
                    fb2 as *mut *mut core::ffi::c_void,
                )
            })?;
            Ok(fb.map(|p| NonNull::new(p.cast())))
        }
    }

    impl Drop for DpiPanel {
        fn drop(&mut self) {
            if self.dma2d {
                unsafe { sys::esp_lcd_dpi_panel_disable_dma2d(self.panel.handle) };
            }
        }
    }
}
