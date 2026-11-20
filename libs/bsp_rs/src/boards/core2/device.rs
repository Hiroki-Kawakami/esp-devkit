/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec;
use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::{error, info, warn};
use idf::sync::OnceLock;
use idf::{Error, Result, spi, thread};

use super::audio::AudioDriver;
use super::power::{self as core2_power, PowerDriver};
use super::{BUTTON_ZONES, PANEL_PPI, PANEL_SIZE, TAG, resolve_rotation};
use crate::audio::{self, Audio};
use crate::button;
use crate::devices::axp192::{self, Axp192, Rail};
use crate::devices::axp2101::{self, Axp2101, Ldo};
use crate::devices::bm8563::{self, Bm8563};
use crate::devices::ft6336u::{self, Ft6336u};
use crate::devices::ili9342c;
use crate::display::{self, Rotation};
use crate::driver::spi_lcd::SpiLcd;
use crate::power;
use crate::rtc::{self, Rtc};
use crate::touch::{self, Touch, ZoneButtons};
use crate::{Board, Config};

pub(crate) type Display = SpiLcd;

const I2C_PORT: i32 = 0;
const I2C_PIN_SDA: i32 = 21;
const I2C_PIN_SCL: i32 = 22;

const LCD_SPI_HOST: i32 = 2;
const LCD_PIN_MOSI: i32 = 23;
const LCD_PIN_SCLK: i32 = 18;
const LCD_PIN_DC: i32 = 15;
const LCD_PIN_CS: i32 = 5;
const LCD_MAX_TRANSFER: usize = 16 * 1024;
const LCD_MADCTL: u8 = 0x08;

const BACKLIGHT_MIN_MV: u32 = 2500;
const BACKLIGHT_MAX_MV: u32 = 3300;

static I2C: OnceLock<MasterBus> = OnceLock::new();
static PMU: OnceLock<Pmu> = OnceLock::new();
static DISPLAY: OnceLock<display::Display> = OnceLock::new();

/// v1.0 has an AXP192, v1.1 an AXP2101 at the same address; the rails and
/// pins the board wires to them differ.
pub(super) enum Pmu {
    Axp192(Axp192<'static>),
    Axp2101(Axp2101<'static>),
}

impl Pmu {
    fn detect(bus: &'static MasterBus) -> Result<Self> {
        match Axp2101::new(bus, axp2101::ADDR, axp2101::DEFAULT_HZ) {
            Ok(axp) => return Ok(Self::Axp2101(axp)),
            Err(err) if err != Error::NOT_FOUND => return Err(err),
            Err(_) => {}
        }
        Axp192::new(bus, axp192::ADDR, axp192::DEFAULT_HZ).map(Self::Axp192)
    }

    fn panel_power_on(&self) -> Result<()> {
        match self {
            Self::Axp192(axp) => {
                axp.set_rail_mv(Rail::Ldo2, 3300)?;
                axp.set_rail_enabled(Rail::Ldo2, true)
            }
            // Both start at voltages fused into the chip, which M5's firmware
            // keeps too.
            Self::Axp2101(axp) => {
                axp.set_ldo_enabled(Ldo::Aldo2, true)?;
                axp.set_ldo_enabled(Ldo::Aldo4, true)
            }
        }
    }

    /// The reset line is shared with the touch controller.
    fn set_panel_reset(&self, held: bool) -> Result<()> {
        match self {
            Self::Axp192(axp) => axp.set_gpio4(!held),
            Self::Axp2101(axp) => axp.set_ldo_enabled(Ldo::Aldo2, !held),
        }
    }

    fn set_speaker(&self, on: bool) -> Result<()> {
        match self {
            Self::Axp192(axp) => axp.set_gpio2(on),
            Self::Axp2101(axp) => {
                if on {
                    axp.set_ldo_mv(Ldo::Aldo3, 3300)?;
                }
                axp.set_ldo_enabled(Ldo::Aldo3, on)
            }
        }
    }

    fn set_backlight(&self, level: u8) -> Result<()> {
        let mv = BACKLIGHT_MIN_MV + (BACKLIGHT_MAX_MV - BACKLIGHT_MIN_MV) * u32::from(level.clamp(1, 100) - 1) / 99;
        match self {
            Self::Axp192(axp) if level == 0 => axp.set_rail_enabled(Rail::Dcdc3, false),
            Self::Axp192(axp) => {
                axp.set_rail_mv(Rail::Dcdc3, mv)?;
                axp.set_rail_enabled(Rail::Dcdc3, true)
            }
            Self::Axp2101(axp) if level == 0 => axp.set_ldo_enabled(Ldo::Bldo1, false),
            Self::Axp2101(axp) => {
                axp.set_ldo_mv(Ldo::Bldo1, mv)?;
                axp.set_ldo_enabled(Ldo::Bldo1, true)
            }
        }
    }
}

pub(crate) fn init(config: &Config) -> Result<Board> {
    let rotation = resolve_rotation(config)?;
    let bus = I2C
        .get_or_try_init(|| {
            MasterBus::new(&BusConfig {
                port: I2C_PORT,
                sda: I2C_PIN_SDA,
                scl: I2C_PIN_SCL,
                glitch_ignore_cnt: 7,
                internal_pullup: true,
            })
        })
        .inspect_err(|err| error!(TAG, "i2c bus: {err}"))?;
    let pmu = PMU.get_or_try_init(|| Pmu::detect(bus)).inspect_err(|err| error!(TAG, "power management chip: {err}"))?;
    let version = match pmu {
        Pmu::Axp192(_) => "v1.0",
        Pmu::Axp2101(_) => "v1.1",
    };
    info!(TAG, "board: Core2 {version}");
    if let Err(err) = core2_power::configure(pmu) {
        warn!(TAG, "power setup: {err}");
    }
    let display = panel_init(rotation, pmu).inspect_err(|err| error!(TAG, "panel: {err}"))?;
    let touch = touch_init(bus, rotation).inspect_err(|err| warn!(TAG, "touch unavailable: {err}")).ok();
    let buttons = touch.and_then(|_| button::start(vec![button::Provider::Level(Box::new(ZoneButtons))]).inspect_err(|err| warn!(TAG, "buttons unavailable: {err}")).ok());
    let audio = audio_init(config, pmu);
    let rtc = rtc_init(bus);
    let power = power::start(PowerDriver::new(pmu), audio);
    Ok(Board {
        audio,
        buttons,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(display))),
        imu: None,
        power,
        rtc,
        sd: None,
        touch,
    })
}

// Non-fatal: the board comes up without audio.
fn audio_init(config: &Config, pmu: &'static Pmu) -> Option<&'static Audio> {
    AudioDriver::new(Box::new(move |on| pmu.set_speaker(on)))
        .and_then(|driver| audio::start(driver, &config.audio))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok()
}

// Non-fatal: the board comes up without a clock. The clock's INT goes to the
// PMIC, which a timer left from before would wake or keep on.
fn rtc_init(bus: &'static MasterBus) -> Option<&'static Rtc> {
    let rtc = Bm8563::new(bus, bm8563::ADDR)
        .and_then(rtc::start)
        .inspect_err(|err| warn!(TAG, "rtc unavailable: {err}"))
        .ok()?;
    if let Err(err) = rtc.stop_timer() {
        error!(TAG, "rtc timer stop: {err}");
    }
    Some(rtc)
}

// The INT line does not read reliably on this board, so the chip is polled.
// Its reset is the panel's, done by then.
fn touch_init(bus: &'static MasterBus, rotation: Rotation) -> Result<&'static Touch> {
    let ft = Ft6336u::new(bus, ft6336u::DEFAULT_HZ)?;
    touch::start(ft, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        rotation,
        int: touch::Int::None,
        zones: &BUTTON_ZONES,
    })
}

fn reset_panel(pmu: &Pmu) -> Result<()> {
    let ms = |ms| thread::sleep(Duration::from_millis(ms));
    pmu.set_panel_reset(true)?;
    ms(10);
    pmu.set_panel_reset(false)?;
    ms(120);
    Ok(())
}

fn panel_init(rotation: Rotation, pmu: &'static Pmu) -> Result<SpiLcd> {
    pmu.panel_power_on()?;
    let bus = spi::Bus::new(&spi::BusConfig {
        host: LCD_SPI_HOST,
        sclk: LCD_PIN_SCLK,
        mosi: LCD_PIN_MOSI,
        miso: None,
        quad: None,
        max_transfer_bytes: LCD_MAX_TRANSFER,
    })?;
    reset_panel(pmu)?;
    ili9342c::create(ili9342c::Config {
        bus,
        cs: LCD_PIN_CS,
        dc: LCD_PIN_DC,
        size: PANEL_SIZE,
        ppi: PANEL_PPI,
        madctl: LCD_MADCTL,
        invert: true,
        rotation,
        backlight: Some(Box::new(move |level| pmu.set_backlight(level))),
    })
}
