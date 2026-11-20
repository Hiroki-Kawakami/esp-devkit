/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::{error, warn};
use idf::spi;
use idf::sync::{Mutex, OnceLock};
use idf::{Error, Result, gpio, thread};

use super::audio::AudioDriver;
use super::power::PowerDriver;
use super::{PANEL_PPI, PANEL_SIZE, TAG, resolve_rotation};
use crate::audio::{self, Audio};
use crate::button::{self, Buttons, Provider};
use crate::devices::co5300;
use crate::devices::cst820::{self, Cst820};
use crate::devices::cst9217::{self, Cst9217};
use crate::devices::m5ioe1::M5Ioe1;
use crate::devices::m5pm1::{M5pm1, PowerButton};
use crate::display::{self, Rotation, Size};
use crate::driver::gpio_button::{GpioButtons, Pin};
use crate::driver::spi_lcd::SpiLcd;
use crate::power;
use crate::touch::{self, Touch};
use crate::{Board, Config};

pub(crate) type Display = SpiLcd;

const I2C_PORT: i32 = 0;
const I2C_PIN_SDA: i32 = 47;
const I2C_PIN_SCL: i32 = 48;

// The schematic puts it at 0x4F; M5Stack's own firmware falls back to 0x6F.
const IOE_ADDRS: [u16; 2] = [0x4F, 0x6F];
const IOE_AU_EN: u8 = 3;
const IOE_TP_RST: u8 = 4;
const IOE_OLED_RST: u8 = 5;
const IOE_L3B_EN: u8 = 8;
const IOE_SPK_EN: u8 = 10;
const L3B_RETRIES: u32 = 10;
const L3B_SETTLE: Duration = Duration::from_millis(80);

const LCD_SPI_HOST: i32 = 1;
const LCD_PIN_CS: i32 = 39;
const LCD_PIN_SCLK: i32 = 40;
const LCD_PIN_D0: i32 = 41;
const LCD_PIN_D1: i32 = 42;
const LCD_PIN_D2: i32 = 46;
const LCD_PIN_D3: i32 = 45;
const LCD_MAX_TRANSFER: usize = 32 * 1024;
const LCD_OFFSET: (u16, u16) = (6, 0);
// The panel flips rows within the 466 lines it scans, not all 480 of its
// memory: the picture stays at row 0 when turned.
const LCD_MEMORY: Size = Size { width: 480, height: 466 };

const TOUCH_PIN_INT: i32 = 13;
const TOUCH_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

// KEYA, then KEYB; pulled up on the board.
static KEYS: [Pin; 2] = [Pin { gpio: 2, active_low: true, pull: false }, Pin { gpio: 1, active_low: true, pull: false }];
const PM1_IRQ_OUT: u8 = 1;
const PM1_PIN_IRQ: i32 = 12;

static I2C: OnceLock<MasterBus> = OnceLock::new();
static IOE: OnceLock<Mutex<M5Ioe1<'static>>> = OnceLock::new();
static PM1: OnceLock<Mutex<M5pm1<'static>>> = OnceLock::new();
static DISPLAY: OnceLock<display::Display> = OnceLock::new();

struct UnknownPower;

impl power::Driver for UnknownPower {}

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
    let ioe = IOE.get_or_try_init(|| expander_init(bus).map(Mutex::new)).inspect_err(|err| error!(TAG, "m5ioe1: {err}"))?;
    let display = panel_init(rotation, ioe).inspect_err(|err| error!(TAG, "panel: {err}"))?;
    let touch = touch_init(bus, ioe, rotation).inspect_err(|err| warn!(TAG, "touch unavailable: {err}")).ok();
    let pm1 = PM1.get_or_try_init(|| M5pm1::new(bus).map(Mutex::new)).inspect_err(|err| warn!(TAG, "m5pm1 unavailable: {err}")).ok();
    let buttons = buttons_init(pm1).inspect_err(|err| warn!(TAG, "buttons unavailable: {err}")).ok();
    let audio = audio_init(config, bus, ioe).inspect_err(|err| warn!(TAG, "audio unavailable: {err}")).ok();
    let power = match pm1.map(PowerDriver::new) {
        Some(Ok(driver)) => power::start(driver, audio),
        Some(Err(err)) => {
            warn!(TAG, "power: {err}");
            power::start(UnknownPower, audio)
        }
        None => power::start(UnknownPower, audio),
    };
    Ok(Board {
        audio,
        buttons,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(display))),
        imu: None,
        power,
        rtc: None,
        sd: None,
        touch,
    })
}

fn expander_init(bus: &'static MasterBus) -> Result<M5Ioe1<'static>> {
    let mut last = Error::NOT_FOUND;
    for addr in IOE_ADDRS {
        match M5Ioe1::new(bus, addr) {
            Ok(ioe) => return Ok(ioe),
            Err(err) => last = err,
        }
    }
    Err(last)
}

// M5Stack's own firmware reads the rail enable back and drives it again until
// it holds, so this does too.
fn panel_power_on(ioe: &mut M5Ioe1) -> Result<()> {
    ioe.set_output_mode(IOE_L3B_EN, true)?;
    for _ in 0..L3B_RETRIES {
        thread::sleep(L3B_SETTLE);
        if ioe.level(IOE_L3B_EN)? {
            return Ok(());
        }
        warn!(TAG, "panel rail enable did not hold; driving it again");
        ioe.set_output(IOE_L3B_EN, true)?;
    }
    Err(Error::TIMEOUT)
}

fn reset_panel(ioe: &mut M5Ioe1) -> Result<()> {
    let ms = |ms| thread::sleep(Duration::from_millis(ms));
    ioe.set_output_mode(IOE_OLED_RST, true)?;
    ms(10);
    ioe.set_output(IOE_OLED_RST, false)?;
    ms(10);
    ioe.set_output(IOE_OLED_RST, true)?;
    ms(10);
    Ok(())
}

fn panel_init(rotation: Rotation, ioe: &Mutex<M5Ioe1>) -> Result<SpiLcd> {
    {
        let mut ioe = ioe.lock();
        panel_power_on(&mut ioe).inspect_err(|err| error!(TAG, "panel power: {err}"))?;
        reset_panel(&mut ioe)?;
    }
    let bus = spi::Bus::new(&spi::BusConfig {
        host: LCD_SPI_HOST,
        sclk: LCD_PIN_SCLK,
        mosi: LCD_PIN_D0,
        miso: Some(LCD_PIN_D1),
        quad: Some([LCD_PIN_D2, LCD_PIN_D3]),
        max_transfer_bytes: LCD_MAX_TRANSFER,
    })?;
    co5300::create(co5300::Config {
        bus,
        cs: LCD_PIN_CS,
        size: PANEL_SIZE,
        offset: LCD_OFFSET,
        memory: LCD_MEMORY,
        ppi: PANEL_PPI,
        rotation,
    })
}

fn reset_touch(ioe: &mut M5Ioe1) -> Result<()> {
    let ms = |ms| thread::sleep(Duration::from_millis(ms));
    ioe.set_output_mode(IOE_TP_RST, false)?;
    ms(10);
    ioe.set_output(IOE_TP_RST, true)?;
    ms(50);
    Ok(())
}

fn touch_init(bus: &'static MasterBus, ioe: &Mutex<M5Ioe1>, rotation: Rotation) -> Result<&'static Touch> {
    reset_touch(&mut ioe.lock())?;
    gpio::set_direction(TOUCH_PIN_INT, gpio::Mode::Input)?;
    gpio::set_pull(TOUCH_PIN_INT, gpio::Pull::Up)?;
    let config = touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        rotation,
        int: touch::Int::Gpio(TOUCH_PIN_INT),
        zones: &[],
    };
    if bus.probe(cst820::ADDR, TOUCH_PROBE_TIMEOUT).is_ok() {
        touch::start(Cst820::new(bus, cst820::DEFAULT_HZ)?, config)
    } else if bus.probe(cst9217::ADDR, TOUCH_PROBE_TIMEOUT).is_ok() {
        touch::start(Cst9217::new(bus, cst9217::DEFAULT_HZ)?, config)
    } else {
        error!(TAG, "no touch controller on 0x{:02x} or 0x{:02x}", cst820::ADDR, cst9217::ADDR);
        Err(Error::NOT_FOUND)
    }
}

fn audio_init(config: &Config, bus: &'static MasterBus, ioe: &'static Mutex<M5Ioe1<'static>>) -> Result<&'static Audio> {
    {
        let mut ioe = ioe.lock();
        ioe.set_output_mode(IOE_SPK_EN, false)?;
        ioe.set_output_mode(IOE_AU_EN, false)?;
    }
    let power = Box::new(move |on| ioe.lock().set_output(IOE_AU_EN, on));
    let amp = Box::new(move |on| ioe.lock().set_output(IOE_SPK_EN, on));
    audio::start(AudioDriver::new(bus, power, amp)?, &config.audio)
}

fn buttons_init(pm1: Option<&'static Mutex<M5pm1<'static>>>) -> Result<&'static Buttons> {
    let mut providers = Vec::new();
    providers.push(Provider::Level(Box::new(GpioButtons::new(&KEYS)?)));
    if let Some(pm1) = pm1 {
        match PowerButton::new(pm1, PM1_IRQ_OUT, PM1_PIN_IRQ) {
            Ok(pwr) => providers.push(Provider::Judged(Box::new(pwr))),
            Err(err) => warn!(TAG, "power button unavailable: {err}"),
        }
    }
    button::start(providers)
}
