/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::error;
use idf::spi;
use idf::sync::{Mutex, OnceLock};
use idf::{Result, thread};

use super::{PANEL_SIZE, TAG, UnknownPower, resolve_rotation};
use crate::devices::aw9523::{Aw9523, PinConfig};
use crate::devices::axp2101::{self, Axp2101, Ldo};
use crate::devices::ili9342c;
use crate::display::{self, Rotation};
use crate::driver::spi_lcd::SpiLcd;
use crate::power;
use crate::{Board, Config};

pub(crate) type Display = SpiLcd;

const I2C_PORT: i32 = 0;
const I2C_PIN_SDA: i32 = 12;
const I2C_PIN_SCL: i32 = 11;

const AW9523_ADDR: u16 = 0x58;
const AW9523_LCD_RST: u8 = 9;
const AW9523_TOUCH_INT: usize = 10;

const LCD_SPI_HOST: i32 = 1;
const LCD_PIN_MOSI: i32 = 37;
const LCD_PIN_SCLK: i32 = 36;
const LCD_PIN_DC: i32 = 35;
const LCD_PIN_CS: i32 = 3;
const LCD_MAX_TRANSFER: usize = 16 * 1024;
const LCD_MADCTL: u8 = 0x08;

const BACKLIGHT_MIN_MV: u32 = 2500;
const BACKLIGHT_MAX_MV: u32 = 3300;
const BACKLIGHT_STEP_MV: u32 = 100;

static I2C: OnceLock<MasterBus> = OnceLock::new();
static AXP2101: OnceLock<Axp2101<'static>> = OnceLock::new();
static AW9523: OnceLock<Mutex<Aw9523<'static>>> = OnceLock::new();
static DISPLAY: OnceLock<display::Display> = OnceLock::new();

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
    let axp = AXP2101
        .get_or_try_init(|| Axp2101::new(bus, axp2101::ADDR, axp2101::DEFAULT_HZ))
        .inspect_err(|err| error!(TAG, "axp2101: {err}"))?;
    rails_init(axp).inspect_err(|err| error!(TAG, "power rails: {err}"))?;
    let aw = AW9523
        .get_or_try_init(|| expander_init(bus).map(Mutex::new))
        .inspect_err(|err| error!(TAG, "aw9523: {err}"))?;
    let display = panel_init(rotation, axp, aw).inspect_err(|err| error!(TAG, "panel: {err}"))?;
    let power = power::start(UnknownPower, None);
    Ok(Board {
        audio: None,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(display))),
        imu: None,
        power,
        rtc: None,
        sd: None,
        touch: None,
    })
}

fn rails_init(axp: &Axp2101) -> Result<()> {
    axp.set_ldo_mv(Ldo::Aldo3, 3300)?;
    axp.set_ldo_mv(Ldo::Aldo4, 3300)?;
    for ldo in [Ldo::Aldo1, Ldo::Aldo2, Ldo::Aldo3, Ldo::Aldo4, Ldo::Bldo1, Ldo::Bldo2] {
        axp.set_ldo_enabled(ldo, true)?;
    }
    axp.set_ldo_enabled(Ldo::Dldo1, false)
}

// P0_2 powers the speaker amplifier, which does not answer on I2C while it is
// low; the other outputs gate the panel and the touch controller.
fn expander_init(bus: &'static MasterBus) -> Result<Aw9523<'static>> {
    let mut pins = [PinConfig::default(); 16];
    for pin in [0, 2, 8, usize::from(AW9523_LCD_RST)] {
        pins[pin] = PinConfig::output(true);
    }
    for pin in [1, 5, 6, 7, 12, 13, 14, 15] {
        pins[pin] = PinConfig::output(false);
    }
    pins[AW9523_TOUCH_INT] = PinConfig::input();
    Aw9523::new(bus, AW9523_ADDR, &pins)
}

fn reset_panel(aw: &Mutex<Aw9523>) -> Result<()> {
    let ms = |ms| thread::sleep(Duration::from_millis(ms));
    let mut aw = aw.lock();
    aw.set_output(AW9523_LCD_RST, true)?;
    ms(10);
    aw.set_output(AW9523_LCD_RST, false)?;
    ms(10);
    aw.set_output(AW9523_LCD_RST, true)?;
    ms(120);
    Ok(())
}

fn panel_init(rotation: Rotation, axp: &'static Axp2101, aw: &Mutex<Aw9523>) -> Result<SpiLcd> {
    let bus = spi::Bus::new(&spi::BusConfig {
        host: LCD_SPI_HOST,
        sclk: LCD_PIN_SCLK,
        mosi: LCD_PIN_MOSI,
        miso: None,
        max_transfer_bytes: LCD_MAX_TRANSFER,
    })?;
    reset_panel(aw)?;
    ili9342c::create(ili9342c::Config {
        bus,
        cs: LCD_PIN_CS,
        dc: LCD_PIN_DC,
        size: PANEL_SIZE,
        madctl: LCD_MADCTL,
        invert: true,
        rotation,
        backlight: Some(Box::new(move |level| set_backlight(axp, level))),
    })
}

fn set_backlight(axp: &Axp2101, level: u8) -> Result<()> {
    if level == 0 {
        return axp.set_ldo_enabled(Ldo::Dldo1, false);
    }
    let steps = (BACKLIGHT_MAX_MV - BACKLIGHT_MIN_MV) / BACKLIGHT_STEP_MV + 1;
    let mv = BACKLIGHT_MIN_MV + BACKLIGHT_STEP_MV * ((u32::from(level.min(100)) - 1) * steps / 100);
    axp.set_ldo_mv(Ldo::Dldo1, mv)?;
    axp.set_ldo_enabled(Ldo::Dldo1, true)
}
