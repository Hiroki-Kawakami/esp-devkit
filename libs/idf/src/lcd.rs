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

pub mod spi {
    use alloc::boxed::Box;
    use core::ffi::c_void;
    use core::mem;
    use core::ptr;
    use core::sync::atomic::{AtomicU32, Ordering};

    use super::PanelIo;
    use crate::spi::Bus;
    use crate::sync::{Mutex, MutexGuard, Semaphore};
    use crate::{Error, Result, sys};

    const MAX_WAKES: u32 = 64;

    pub struct IoConfig {
        pub cs: i32,
        pub dc: i32,
        pub pclk_hz: u32,
        pub queue_depth: usize,
        pub cmd_bits: i32,
        pub param_bits: i32,
    }

    struct Done {
        completed: AtomicU32,
        waiters: AtomicU32,
        wake: Semaphore,
    }

    impl Done {
        fn reached(&self, ticket: u32) -> bool {
            self.completed.load(Ordering::SeqCst).wrapping_sub(ticket) as i32 >= 0
        }
    }

    // The bus interrupt is not allocated IRAM-safe, so this may stay in flash.
    unsafe extern "C" fn color_done(
        _io: sys::esp_lcd_panel_io_handle_t,
        _data: *mut sys::esp_lcd_panel_io_event_data_t,
        ctx: *mut c_void,
    ) -> bool {
        let done = unsafe { &*ctx.cast::<Done>() };
        done.completed.fetch_add(1, Ordering::SeqCst);
        for _ in 0..done.waiters.load(Ordering::SeqCst) {
            done.wake.give_from_isr();
        }
        false
    }

    /// A panel IO on an SPI bus whose color transfers run in the background,
    /// in the order they were queued.
    pub struct Io {
        io: PanelIo,
        queued: Mutex<u32>,
        done: Box<Done>,
    }

    /// The IO for one task: what it sends meanwhile stays together.
    pub struct IoGuard<'a> {
        io: &'a PanelIo,
        queued: MutexGuard<'a, u32>,
    }

    impl Io {
        pub fn new(bus: &Bus, config: &IoConfig) -> Result<Self> {
            let done = Box::new(Done {
                completed: AtomicU32::new(0),
                waiters: AtomicU32::new(0),
                wake: Semaphore::try_new(0, MAX_WAKES)?,
            });
            let mut cfg: sys::esp_lcd_panel_io_spi_config_t = unsafe { mem::zeroed() };
            cfg.cs_gpio_num = config.cs;
            cfg.dc_gpio_num = config.dc;
            cfg.pclk_hz = config.pclk_hz;
            cfg.trans_queue_depth = config.queue_depth;
            cfg.on_color_trans_done = Some(color_done);
            cfg.user_ctx = (&*done as *const Done).cast_mut().cast();
            cfg.lcd_cmd_bits = config.cmd_bits;
            cfg.lcd_param_bits = config.param_bits;
            let mut handle = ptr::null_mut();
            Error::check(unsafe { sys::esp_lcd_new_panel_io_spi(bus.host(), &cfg, &mut handle) })?;
            let io = PanelIo { handle, _parent: Some(bus.keep()) };
            Ok(Self { io, queued: Mutex::new(0), done })
        }

        pub fn lock(&self) -> IoGuard<'_> {
            IoGuard { io: &self.io, queued: self.queued.lock() }
        }

        /// Blocks until the transfer of `ticket` has finished.
        pub fn wait(&self, ticket: u32) {
            let done = &*self.done;
            if done.reached(ticket) {
                return;
            }
            done.waiters.fetch_add(1, Ordering::SeqCst);
            while !done.reached(ticket) {
                done.wake.take();
            }
            done.waiters.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl IoGuard<'_> {
        /// Waits for the transfers queued before.
        pub fn tx_param(&self, cmd: i32, param: &[u8]) -> Result<()> {
            self.io.tx_param(cmd, param)
        }

        /// Queues `data` after `cmd`, -1 for none, waiting for the transfers
        /// queued before when there is a command. `data` must stay unchanged
        /// until `Io::wait` for the returned ticket returns.
        pub unsafe fn tx_color(&mut self, cmd: i32, data: &[u8]) -> Result<u32> {
            if data.is_empty() {
                return Err(Error::INVALID_ARG);
            }
            Error::check(unsafe {
                sys::esp_lcd_panel_io_tx_color(self.io.handle, cmd, data.as_ptr().cast(), data.len())
            })?;
            *self.queued = self.queued.wrapping_add(1);
            Ok(*self.queued)
        }
    }
}

#[cfg(CONFIG_SOC_LCD_I80_SUPPORTED)]
pub mod i80 {
    use alloc::sync::Arc;
    use core::ffi::c_void;
    use core::mem;
    use core::ptr::{self, NonNull};

    use super::PanelIo;
    use crate::heap::Caps;
    use crate::{Error, Result, sys};

    /// Runs in interrupt context when a color transfer has gone out; returns
    /// whether it woke a higher-priority task.
    pub type ColorDone = unsafe extern "C" fn(
        io: sys::esp_lcd_panel_io_handle_t,
        data: *mut sys::esp_lcd_panel_io_event_data_t,
        ctx: *mut c_void,
    ) -> bool;

    struct BusHandle(sys::esp_lcd_i80_bus_handle_t);

    unsafe impl Send for BusHandle {}
    unsafe impl Sync for BusHandle {}

    impl Drop for BusHandle {
        fn drop(&mut self) {
            unsafe { sys::esp_lcd_del_i80_bus(self.0) };
        }
    }

    pub struct BusConfig<'a> {
        pub dc: i32,
        pub wr: i32,
        /// 8 or 16 lines, D0 first.
        pub data: &'a [i32],
        pub max_transfer_bytes: usize,
        pub dma_burst_size: usize,
    }

    #[derive(Clone, Copy, Default)]
    pub struct DcLevels {
        pub idle: bool,
        pub cmd: bool,
        pub dummy: bool,
        pub data: bool,
    }

    #[derive(Clone, Copy, Default)]
    pub struct IoFlags {
        pub cs_active_high: bool,
        pub reverse_color_bits: bool,
        pub swap_color_bytes: bool,
        pub pclk_active_neg: bool,
        pub pclk_idle_low: bool,
    }

    pub struct IoConfig {
        /// -1 when the device has the bus to itself.
        pub cs: i32,
        pub pclk_hz: u32,
        pub queue_depth: usize,
        pub cmd_bits: i32,
        pub param_bits: i32,
        pub dc_levels: DcLevels,
        pub flags: IoFlags,
    }

    pub struct Bus(Arc<BusHandle>);

    impl Bus {
        /// The bus interrupt lands on the core that calls this.
        pub fn new(config: &BusConfig) -> Result<Self> {
            let mut cfg: sys::esp_lcd_i80_bus_config_t = unsafe { mem::zeroed() };
            if !matches!(config.data.len(), 8 | 16) {
                return Err(Error::INVALID_ARG);
            }
            cfg.dc_gpio_num = config.dc;
            cfg.wr_gpio_num = config.wr;
            cfg.clk_src = sys::soc_periph_lcd_clk_src_t_LCD_CLK_SRC_DEFAULT;
            for (slot, &pin) in cfg.data_gpio_nums.iter_mut().zip(config.data) {
                *slot = pin;
            }
            cfg.bus_width = config.data.len();
            cfg.max_transfer_bytes = config.max_transfer_bytes;
            cfg.dma_burst_size = config.dma_burst_size;
            let mut handle = ptr::null_mut();
            Error::check(unsafe { sys::esp_lcd_new_i80_bus(&cfg, &mut handle) })?;
            Ok(Self(Arc::new(BusHandle(handle))))
        }

        /// `on_color_done` gets `ctx`, which must stay valid as long as the IO.
        pub unsafe fn new_io(&self, config: &IoConfig, on_color_done: Option<ColorDone>, ctx: *mut c_void) -> Result<Io> {
            let mut cfg: sys::esp_lcd_panel_io_i80_config_t = unsafe { mem::zeroed() };
            cfg.cs_gpio_num = config.cs;
            cfg.pclk_hz = config.pclk_hz;
            cfg.trans_queue_depth = config.queue_depth;
            cfg.on_color_trans_done = on_color_done;
            cfg.user_ctx = ctx;
            cfg.lcd_cmd_bits = config.cmd_bits;
            cfg.lcd_param_bits = config.param_bits;
            let levels = config.dc_levels;
            cfg.dc_levels.set_dc_idle_level(levels.idle.into());
            cfg.dc_levels.set_dc_cmd_level(levels.cmd.into());
            cfg.dc_levels.set_dc_dummy_level(levels.dummy.into());
            cfg.dc_levels.set_dc_data_level(levels.data.into());
            let flags = config.flags;
            cfg.flags.set_cs_active_high(flags.cs_active_high.into());
            cfg.flags.set_reverse_color_bits(flags.reverse_color_bits.into());
            cfg.flags.set_swap_color_bytes(flags.swap_color_bytes.into());
            cfg.flags.set_pclk_active_neg(flags.pclk_active_neg.into());
            cfg.flags.set_pclk_idle_low(flags.pclk_idle_low.into());
            let mut handle = ptr::null_mut();
            Error::check(unsafe { sys::esp_lcd_new_panel_io_i80(self.0.0, &cfg, &mut handle) })?;
            Ok(Io(PanelIo { handle, _parent: Some(self.0.clone()) }))
        }
    }

    pub struct Io(PanelIo);

    impl Io {
        pub fn tx_param(&self, cmd: i32, param: &[u8]) -> Result<()> {
            self.0.tx_param(cmd, param)
        }

        /// Queues `data` after `cmd`, -1 for none. `data` must stay unchanged
        /// until the color-done callback for it has run.
        pub unsafe fn tx_color(&self, cmd: i32, data: &[u8]) -> Result<()> {
            Error::check(unsafe { sys::esp_lcd_panel_io_tx_color(self.0.handle, cmd, data.as_ptr().cast(), data.len()) })
        }

        /// A buffer this IO's DMA can send, aligned as it needs.
        pub fn alloc_buffer(&self, len: usize, caps: Caps) -> Result<DmaBuffer> {
            let ptr = unsafe { sys::esp_lcd_i80_alloc_draw_buffer(self.0.handle, len, caps.bits()) };
            let ptr = NonNull::new(ptr.cast::<u8>()).ok_or(Error::NO_MEM)?;
            unsafe { ptr.as_ptr().write_bytes(0, len) };
            Ok(DmaBuffer { ptr, len })
        }
    }

    /// Zeroed when allocated.
    pub struct DmaBuffer {
        ptr: NonNull<u8>,
        len: usize,
    }

    unsafe impl Send for DmaBuffer {}
    unsafe impl Sync for DmaBuffer {}

    impl DmaBuffer {
        pub fn as_ptr(&self) -> *mut u8 {
            self.ptr.as_ptr()
        }

        pub fn len(&self) -> usize {
            self.len
        }

        pub fn is_empty(&self) -> bool {
            self.len == 0
        }
    }

    impl Drop for DmaBuffer {
        fn drop(&mut self) {
            unsafe { sys::heap_caps_free(self.ptr.as_ptr().cast()) };
        }
    }
}

#[cfg(CONFIG_SOC_MIPI_DSI_SUPPORTED)]
pub mod dsi {
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use core::ffi::c_void;
    use core::mem;
    use core::ptr::{self, NonNull};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::time::Duration;

    use super::{Panel, PanelIo};
    use crate::sync::Semaphore;
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
                sync: None,
            })
        }
    }

    struct FrameSync {
        queued: AtomicUsize,
        scanning: AtomicUsize,
        started: Semaphore,
    }

    // Flash code in a 2D-DMA ISR path faults at cache-line boundaries on P4.
    #[unsafe(link_section = ".iram1.idf_dpi_frame_started")]
    unsafe extern "C" fn frame_started(
        _panel: sys::esp_lcd_panel_handle_t,
        _data: *mut sys::esp_lcd_dpi_panel_event_data_t,
        ctx: *mut c_void,
    ) -> bool {
        let sync = unsafe { &*ctx.cast::<FrameSync>() };
        sync.scanning.store(sync.queued.load(Ordering::Relaxed), Ordering::Release);
        let mut woken = 0;
        unsafe { sys::xQueueGiveFromISR(sync.started.raw(), &mut woken) };
        woken != 0
    }

    pub struct DpiPanel {
        panel: Panel,
        num_fbs: u8,
        dma2d: bool,
        sync: Option<Box<FrameSync>>,
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

        pub fn enable_frame_sync(&mut self) -> Result<()> {
            let sync = Box::new(FrameSync {
                queued: AtomicUsize::new(0),
                scanning: AtomicUsize::new(0),
                started: Semaphore::try_new(0, 1)?,
            });
            let mut cbs: sys::esp_lcd_dpi_panel_event_callbacks_t = unsafe { mem::zeroed() };
            cbs.__bindgen_anon_1.on_frame_buf_complete = Some(frame_started);
            let ctx = (&*sync as *const FrameSync).cast_mut().cast();
            Error::check(unsafe { sys::esp_lcd_dpi_panel_register_event_callbacks(self.panel.handle, &cbs, ctx) })?;
            self.sync = Some(sync);
            Ok(())
        }

        /// The framebuffer the panel scans from the next frame boundary on.
        pub fn set_queued(&self, index: usize) {
            if let Some(sync) = &self.sync {
                sync.queued.store(index, Ordering::Relaxed);
            }
        }

        pub fn scanning(&self) -> Option<usize> {
            Some(self.sync.as_ref()?.scanning.load(Ordering::Acquire))
        }

        /// `false` on timeout, or without `enable_frame_sync`.
        pub fn wait_frame(&self, timeout: Duration) -> bool {
            let Some(sync) = &self.sync else { return false };
            sync.started.try_take();
            sync.started.take_timeout(timeout)
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
