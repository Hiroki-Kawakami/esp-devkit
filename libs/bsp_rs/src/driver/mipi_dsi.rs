/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use idf::lcd::PanelIo;
use idf::lcd::dsi::{self, ColorFormat, DpiPanel, VideoTiming};
use idf::log::error;
use idf::{Error, Result, ldo, ledc, thread};

use crate::display::{Driver, FrameBuffer, ImageFormat, Size};

const TAG: &CStr = c"mipi_dsi";

const BACKLIGHT_TIMER: u32 = 0;
const BACKLIGHT_CHANNEL: u32 = 0;
const BACKLIGHT_BITS: u32 = 12;
const BACKLIGHT_MAX_DUTY: u32 = (1 << BACKLIGHT_BITS) - 1;
const PHY_LDO_CHANNEL: i32 = 3;
const PHY_LDO_MV: i32 = 2500;
const FRAME_TIMEOUT: Duration = Duration::from_millis(200);

pub(crate) struct InitCmd<'a> {
    pub cmd: u8,
    pub data: &'a [u8],
    pub delay_ms: u32,
}

pub(crate) fn send_init_cmds(io: &PanelIo, cmds: &[InitCmd]) -> Result<()> {
    for c in cmds {
        if let Err(err) = io.tx_param(i32::from(c.cmd), c.data) {
            error!(TAG, "init cmd 0x{:02x} failed: {err}", c.cmd);
            return Err(err);
        }
        if c.delay_ms > 0 {
            thread::sleep(Duration::from_millis(u64::from(c.delay_ms)));
        }
    }
    Ok(())
}

pub(crate) struct Config {
    pub size: Size,
    pub pixel_format: ImageFormat,
    /// 0 selects one framebuffer.
    pub fb_num: u8,
    pub backlight_gpio: Option<i32>,
    /// 12-bit duty at brightness 1; 0 means 1.
    pub backlight_min_duty: u16,
    pub lane_bit_rate_mbps_rgb565: f32,
    pub lane_bit_rate_mbps_rgb888: f32,
    pub dpi_clock_freq_mhz: f32,
    /// `h_size`/`v_size` are replaced by `size`.
    pub video_timing: VideoTiming,
    pub panel_init: fn(&PanelIo, ImageFormat) -> Result<()>,
}

struct Stack {
    panel: DpiPanel,
    _io: PanelIo,
    _bus: dsi::Bus,
    frame_buffers: [Option<NonNull<u8>>; dsi::MAX_FRAME_BUFFERS],
}

pub(crate) struct MipiDsi {
    backlight: Option<ledc::Channel>,
    backlight_min_duty: u32,
    stack: Option<Stack>,
    _phy_power: ldo::Channel,
    size: Size,
    format: ImageFormat,
    fb_num: u8,
    shown: AtomicUsize,
    lane_bit_rate_mbps_rgb565: f32,
    lane_bit_rate_mbps_rgb888: f32,
    dpi_clock_freq_mhz: f32,
    video_timing: VideoTiming,
    panel_init: fn(&PanelIo, ImageFormat) -> Result<()>,
}

unsafe impl Send for MipiDsi {}
unsafe impl Sync for MipiDsi {}

fn color_format(format: ImageFormat) -> Result<ColorFormat> {
    match format {
        ImageFormat::Rgb565 => Ok(ColorFormat::Rgb565),
        ImageFormat::Bgr888 => Ok(ColorFormat::Rgb888),
        _ => Err(Error::NOT_SUPPORTED),
    }
}

impl MipiDsi {
    pub(crate) fn new(config: &Config) -> Result<Self> {
        color_format(config.pixel_format)?;
        if config.size.width == 0 || config.size.height == 0
            || usize::from(config.fb_num) > dsi::MAX_FRAME_BUFFERS
            || u32::from(config.backlight_min_duty) > BACKLIGHT_MAX_DUTY + 1 - 100
        {
            return Err(Error::INVALID_ARG);
        }

        let backlight = match config.backlight_gpio {
            Some(gpio) => {
                ledc::configure_timer(&ledc::TimerConfig {
                    timer: BACKLIGHT_TIMER,
                    freq_hz: 5000,
                    resolution_bits: BACKLIGHT_BITS,
                })?;
                Some(ledc::Channel::new(&ledc::ChannelConfig {
                    channel: BACKLIGHT_CHANNEL,
                    timer: BACKLIGHT_TIMER,
                    gpio,
                    duty: 0,
                })?)
            }
            None => None,
        };

        let phy_power = ldo::Channel::acquire(PHY_LDO_CHANNEL, PHY_LDO_MV)?;

        let mut dsi = Self {
            backlight,
            backlight_min_duty: u32::from(config.backlight_min_duty.max(1)),
            stack: None,
            _phy_power: phy_power,
            size: config.size,
            format: config.pixel_format,
            fb_num: config.fb_num.max(1),
            shown: AtomicUsize::new(0),
            lane_bit_rate_mbps_rgb565: config.lane_bit_rate_mbps_rgb565,
            lane_bit_rate_mbps_rgb888: config.lane_bit_rate_mbps_rgb888,
            dpi_clock_freq_mhz: config.dpi_clock_freq_mhz,
            video_timing: config.video_timing,
            panel_init: config.panel_init,
        };
        dsi.stack = Some(dsi.start(dsi.format, dsi.fb_num)?);
        Ok(dsi)
    }

    fn start(&self, format: ImageFormat, fb_num: u8) -> Result<Stack> {
        let color = color_format(format)?;
        let bus = dsi::Bus::new(&dsi::BusConfig {
            bus_id: 0,
            num_data_lanes: 2,
            lane_bit_rate_mbps: match color {
                ColorFormat::Rgb565 => self.lane_bit_rate_mbps_rgb565,
                ColorFormat::Rgb888 => self.lane_bit_rate_mbps_rgb888,
            },
        })?;
        let io = bus.new_dbi_io(0, 8, 8)?;
        let mut panel = bus.new_dpi_panel(&dsi::DpiPanelConfig {
            virtual_channel: 0,
            dpi_clock_freq_mhz: self.dpi_clock_freq_mhz,
            in_color_format: color,
            out_color_format: color,
            num_fbs: fb_num,
            video_timing: VideoTiming {
                h_size: u32::from(self.size.width),
                v_size: u32::from(self.size.height),
                ..self.video_timing
            },
        })?;
        panel.enable_dma2d()?;
        panel.enable_frame_sync()?;
        (self.panel_init)(&io, format)?;
        panel.panel().init()?;
        let frame_buffers = panel.frame_buffers()?;
        Ok(Stack { panel, _io: io, _bus: bus, frame_buffers })
    }
}

impl Driver for MipiDsi {
    fn size(&self) -> Size {
        self.size
    }

    fn pixel_format(&self) -> ImageFormat {
        self.format
    }

    fn frame_buffer_count(&self) -> usize {
        usize::from(self.fb_num)
    }

    fn frame_buffer(&self, index: usize) -> Option<FrameBuffer> {
        let ptr = (*self.stack.as_ref()?.frame_buffers.get(index)?)?;
        Some(unsafe { FrameBuffer::new(ptr, self.size, self.format) })
    }

    fn present(&self, index: usize) -> Result<()> {
        let stack = self.stack.as_ref().ok_or(Error::INVALID_STATE)?;
        let fb = stack.frame_buffers.get(index).copied().flatten().ok_or(Error::INVALID_ARG)?;
        self.shown.store(index, Ordering::Relaxed);
        unsafe {
            stack.panel.panel().draw_bitmap(
                0,
                0,
                i32::from(self.size.width),
                i32::from(self.size.height),
                fb.as_ptr().cast(),
            )
        }?;
        stack.panel.set_queued(index);
        Ok(())
    }

    fn scanning(&self) -> Option<usize> {
        self.stack.as_ref()?.panel.scanning()
    }

    fn wait_frame(&self) -> Result<()> {
        let stack = self.stack.as_ref().ok_or(Error::INVALID_STATE)?;
        if stack.panel.wait_frame(FRAME_TIMEOUT) { Ok(()) } else { Err(Error::TIMEOUT) }
    }

    fn shown(&self) -> Option<FrameBuffer> {
        self.frame_buffer(self.shown.load(Ordering::Relaxed))
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        let Some(backlight) = &self.backlight else {
            return Err(Error::NOT_SUPPORTED);
        };
        let level = u32::from(level.min(100));
        let min = self.backlight_min_duty;
        let duty = if level == 0 {
            0
        } else {
            let t = (level - 1) as f32 / 99.0;
            let duty = min + (t * t * (BACKLIGHT_MAX_DUTY - min) as f32 + 0.5) as u32;
            duty.max(min + level - 1)
        };
        backlight.set_duty(duty)
    }

    fn reconfigure(&mut self, format: ImageFormat, fb_num: u8) -> Result<()> {
        color_format(format)?;
        if fb_num == 0 || usize::from(fb_num) > dsi::MAX_FRAME_BUFFERS {
            return Err(Error::INVALID_ARG);
        }
        if format == self.format && fb_num == self.fb_num && self.stack.is_some() {
            return Ok(());
        }
        let duty = self.backlight.as_ref().map(|b| b.duty());
        if let Some(backlight) = &self.backlight {
            backlight.set_duty(0)?;
        }
        self.stack = None;
        self.shown.store(0, Ordering::Relaxed);
        let result = match self.start(format, fb_num) {
            Ok(stack) => {
                (self.stack, self.format, self.fb_num) = (Some(stack), format, fb_num);
                Ok(())
            }
            Err(err) => {
                error!(TAG, "reconfigure to {format:?} x{fb_num}: {err}");
                self.stack = self.start(self.format, self.fb_num).inspect_err(|err| error!(TAG, "restore: {err}")).ok();
                Err(err)
            }
        };
        if let (Some(backlight), Some(duty), true) = (&self.backlight, duty, self.stack.is_some()) {
            if let Err(err) = backlight.set_duty(duty) {
                error!(TAG, "backlight: {err}");
            }
        }
        result
    }
}
