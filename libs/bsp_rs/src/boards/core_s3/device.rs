/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::c_void;
use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::{error, warn};
use idf::spi;
use idf::sync::{Mutex, OnceLock};
use idf::{Result, gpio, thread};

use super::audio::AudioDriver;
use super::power::{self as core_s3_power, AW9523_BOOST_EN, AW9523_USB_OTG_EN, PowerDriver};
use super::{PANEL_PPI, PANEL_SIZE, TAG, resolve_rotation};
use crate::audio::{self, Audio};
use crate::devices::aw9523::{Aw9523, PinConfig};
use crate::devices::axp2101::{self, Axp2101, Ldo};
use crate::devices::bm8563::{self, Bm8563};
use crate::devices::ft6336u::{self, Ft6336u};
use crate::devices::ili9342c;
use crate::dispatch::{self, Next, Notifier, Source};
use crate::display::{self, Rotation};
use crate::driver::spi_lcd::SpiLcd;
use crate::power;
use crate::rtc::{self, Rtc};
use crate::touch::{self, Touch};
use crate::{Board, Config};

pub(crate) type Display = SpiLcd;

const I2C_PORT: i32 = 0;
const I2C_PIN_SDA: i32 = 12;
const I2C_PIN_SCL: i32 = 11;

const AW9523_ADDR: u16 = 0x58;
const AW9523_AMP_RST: u8 = 2;
const AW9523_LCD_RST: u8 = 9;
const AW9523_TOUCH_INT: usize = 10;
const AW9523_PIN_INT: i32 = 21;
const EXPANDER_RETRY: Duration = Duration::from_millis(10);

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
static EXPANDER_INT: OnceLock<Notifier> = OnceLock::new();

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
    if let Err(err) = core_s3_power::configure(axp) {
        warn!(TAG, "charger setup: {err}");
    }
    let aw = AW9523
        .get_or_try_init(|| expander_init(bus).map(Mutex::new))
        .inspect_err(|err| error!(TAG, "aw9523: {err}"))?;
    let display = panel_init(rotation, axp, aw).inspect_err(|err| error!(TAG, "panel: {err}"))?;
    let touch = touch_init(bus, aw, rotation).inspect_err(|err| warn!(TAG, "touch unavailable: {err}")).ok();
    let audio = audio_init(config, bus, aw);
    let rtc = rtc_init(bus);
    let power = power::start(PowerDriver::new(axp, aw), audio);
    Ok(Board {
        audio,
        buttons: None,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(display))),
        imu: None,
        power,
        rtc,
        sd: None,
        touch,
    })
}

// The LDOs start at voltages fused into the chip, and ALDO1 feeds the amp's
// 1.8 V core, which takes no more than 2 V.
fn rails_init(axp: &Axp2101) -> Result<()> {
    axp.set_ldo_mv(Ldo::Aldo1, 1800)?;
    axp.set_ldo_mv(Ldo::Aldo2, 3300)?;
    axp.set_ldo_mv(Ldo::Aldo3, 3300)?;
    axp.set_ldo_mv(Ldo::Aldo4, 3300)?;
    for ldo in [Ldo::Aldo1, Ldo::Aldo2, Ldo::Aldo3, Ldo::Aldo4, Ldo::Bldo1, Ldo::Bldo2] {
        axp.set_ldo_enabled(ldo, true)?;
    }
    axp.set_ldo_enabled(Ldo::Dldo1, false)
}

// The speaker amp stays in reset until a stream needs it; the other outputs
// gate the panel, the touch controller and the 5 V outputs.
fn expander_init(bus: &'static MasterBus) -> Result<Aw9523<'static>> {
    let mut pins = [PinConfig::default(); 16];
    for pin in [0, 8, usize::from(AW9523_LCD_RST)] {
        pins[pin] = PinConfig::output(true);
    }
    for pin in [1, AW9523_AMP_RST, AW9523_USB_OTG_EN, 6, 7, 12, 13, 14, AW9523_BOOST_EN].map(usize::from) {
        pins[pin] = PinConfig::output(false);
    }
    pins[AW9523_TOUCH_INT] = PinConfig::input(true);
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
        quad: None,
        max_transfer_bytes: LCD_MAX_TRANSFER,
    })?;
    reset_panel(aw)?;
    ili9342c::create(ili9342c::Config {
        bus,
        cs: LCD_PIN_CS,
        dc: LCD_PIN_DC,
        size: PANEL_SIZE,
        ppi: PANEL_PPI,
        madctl: LCD_MADCTL,
        invert: true,
        rotation,
        backlight: Some(Box::new(move |level| set_backlight(axp, level))),
    })
}

fn touch_init(bus: &'static MasterBus, aw: &'static Mutex<Aw9523<'static>>, rotation: Rotation) -> Result<&'static Touch> {
    let ft = Ft6336u::new(bus, ft6336u::DEFAULT_HZ)?;
    let touch = touch::start(ft, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        rotation,
        int: touch::Int::External,
        zones: &[],
    })?;
    expander_int_init(aw, touch).inspect_err(|err| error!(TAG, "expander INT on GPIO{AW9523_PIN_INT}: {err}"))?;
    Ok(touch)
}

// The expander holds its INT low until its inputs are read over I2C, which the
// ISR leaves to the dispatch task.
struct ExpanderInt {
    aw: &'static Mutex<Aw9523<'static>>,
    touch: &'static Touch,
    failing: bool,
}

impl Source for ExpanderInt {
    fn tick(&mut self) -> Next {
        if let Err(err) = self.aw.lock().read_inputs() {
            if !self.failing {
                warn!(TAG, "aw9523 inputs: {err}");
                self.failing = true;
            }
            return Next::After(EXPANDER_RETRY);
        }
        self.failing = false;
        self.touch.interrupt();
        Next::Idle
    }
}

fn expander_int_init(aw: &'static Mutex<Aw9523<'static>>, touch: &'static Touch) -> Result<()> {
    let notifier = dispatch::add(Box::new(ExpanderInt { aw, touch, failing: false }))?;
    let _ = EXPANDER_INT.set(notifier);
    gpio::set_direction(AW9523_PIN_INT, gpio::Mode::Input)?;
    gpio::set_pull(AW9523_PIN_INT, gpio::Pull::Up)?;
    gpio::install_isr_service()?;
    gpio::set_interrupt(AW9523_PIN_INT, gpio::Interrupt::FallingEdge)?;
    unsafe { gpio::add_isr_handler(AW9523_PIN_INT, on_expander_int, core::ptr::null_mut()) }
}

unsafe extern "C" fn on_expander_int(_: *mut c_void) {
    if let Some(n) = EXPANDER_INT.get() {
        n.notify_from_isr();
    }
}

// Non-fatal: the board comes up without audio.
fn audio_init(config: &Config, bus: &'static MasterBus, aw: &'static Mutex<Aw9523<'static>>) -> Option<&'static Audio> {
    AudioDriver::new(bus, Box::new(move |held| aw.lock().set_output(AW9523_AMP_RST, !held)))
        .and_then(|driver| audio::start(driver, &config.audio))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok()
}

// Non-fatal: the board comes up without a clock. The clock's INT holds the
// PMIC's IRQ low, which powers a board that is off back on; a timer left from
// before would turn a later power off into a restart.
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

fn set_backlight(axp: &Axp2101, level: u8) -> Result<()> {
    if level == 0 {
        return axp.set_ldo_enabled(Ldo::Dldo1, false);
    }
    let steps = (BACKLIGHT_MAX_MV - BACKLIGHT_MIN_MV) / BACKLIGHT_STEP_MV + 1;
    let mv = BACKLIGHT_MIN_MV + BACKLIGHT_STEP_MV * ((u32::from(level.min(100)) - 1) * steps / 100);
    axp.set_ldo_mv(Ldo::Dldo1, mv)?;
    axp.set_ldo_enabled(Ldo::Dldo1, true)
}
