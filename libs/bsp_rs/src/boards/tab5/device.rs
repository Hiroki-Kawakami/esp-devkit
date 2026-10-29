/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::{error, info, warn};
use idf::sync::OnceLock;
use idf::{Error, Result, gpio, thread};

use super::{PANEL_SIZE, TAG, resolve_fb_num, resolve_pixel_format};
use crate::devices::gt911::{self, Gt911};
use crate::devices::st712x_touch::{self, St712xTouch};
use crate::devices::{ili9881c, st7121, st7123};
use crate::devices::pi4io::{Pi4io, PinConfig};
use crate::display;
use crate::driver::mipi_dsi::MipiDsi;
use crate::touch::{self, Touch};
use crate::{Board, Config};

const I2C_PORT: i32 = 0;
const I2C_PIN_SDA: i32 = 31;
const I2C_PIN_SCL: i32 = 32;

const PI4IOE1_ADDR: u16 = 0x43;
const PI4IOE2_ADDR: u16 = 0x44;
const PI4IOE1_LCD_RST: u8 = 4;
const PI4IOE1_TP_RST: u8 = 5;

const TOUCH_PIN_INT: i32 = 23;

const LCD_PIN_BL: i32 = 22;
// The backlight boost driver holds a fixed current floor while enabled;
// brightness does not change below this duty at 5kHz.
const LCD_BL_MIN_DUTY: u16 = 1300;

crate::display::driver_enum! {
    pub(crate) enum Display {
        Ili9881c(MipiDsi),
        St7121(MipiDsi),
        St7123(MipiDsi),
    }
}

static I2C: OnceLock<MasterBus> = OnceLock::new();
static DISPLAY: OnceLock<display::Display> = OnceLock::new();

pub(crate) fn init(config: &Config) -> Result<Board> {
    let bus = I2C
        .get_or_try_init(|| {
            MasterBus::new(&BusConfig {
                port: I2C_PORT,
                sda: I2C_PIN_SDA,
                scl: I2C_PIN_SCL,
                glitch_ignore_cnt: 0,
                internal_pullup: true,
            })
        })
        .inspect_err(|err| error!(TAG, "i2c bus: {err}"))?;

    let (mut pi4ioe1, _pi4ioe2) =
        io_expanders_init(bus).inspect_err(|err| error!(TAG, "io expanders: {err}"))?;
    reset_panel_and_touch(&mut pi4ioe1)?;

    let display = panel_init(config, bus)?;
    let touch = touch_init(bus, &display);
    Ok(Board { display: Some(DISPLAY.get_or_init(|| display::Display(display))), touch })
}

// PI4IOE1 owns LCD_RST + TP_RST + SPK_EN + the headphone-detect input; PI4IOE2
// owns the power rails. SPK_EN starts LOW so the codec stays muted until audio
// bring-up enables it.
fn io_expanders_init(bus: &MasterBus) -> Result<(Pi4io<'_>, Pi4io<'_>)> {
    let mut pi4ioe1 = [PinConfig::default(); 8];
    pi4ioe1[0] = PinConfig::output(false); // RF_INT_EXT_SWITCH
    pi4ioe1[1] = PinConfig::output(false); // SPK_EN
    pi4ioe1[2] = PinConfig::output(true); // EXT5V_EN
    pi4ioe1[usize::from(PI4IOE1_LCD_RST)] = PinConfig::output(true);
    pi4ioe1[usize::from(PI4IOE1_TP_RST)] = PinConfig::output(true);
    pi4ioe1[6] = PinConfig::output(true); // CAM_RST
    pi4ioe1[7] = PinConfig::input(); // HP_DET
    let pi4ioe1 = Pi4io::new(bus, PI4IOE1_ADDR, &pi4ioe1)?;

    let mut pi4ioe2 = [PinConfig::default(); 8];
    pi4ioe2[0] = PinConfig::output(true); // WLAN_PWR_EN
    pi4ioe2[3] = PinConfig::output(false); // USB5V_EN
    pi4ioe2[4] = PinConfig::output(false); // PWROFF_PLUSE
    pi4ioe2[5] = PinConfig::output(false); // nCHG_QC_EN
    pi4ioe2[6] = PinConfig::input(); // CHG_STAT
    pi4ioe2[7] = PinConfig::output(true); // CHG_EN
    let pi4ioe2 = Pi4io::new(bus, PI4IOE2_ADDR, &pi4ioe2)?;
    Ok((pi4ioe1, pi4ioe2))
}

// The GT911 needs the full AOSP gtp_reset_guitar sequence to boot its touch
// firmware: hold INT high across TP_RST's rising edge (selects 0x14), then drive
// INT low for 100 ms and release. A cold POR hides the omission, but esp_restart
// leaves the touch rail powered, so this is required for touch to survive a warm
// reboot. The panel probe relies on the 0x14 address it latches.
fn reset_panel_and_touch(pi4ioe1: &mut Pi4io) -> Result<()> {
    let ms = |ms| thread::sleep(Duration::from_millis(ms));
    gpio::set_direction(TOUCH_PIN_INT, gpio::Mode::Output)?;
    gpio::set_level(TOUCH_PIN_INT, true)?;
    pi4ioe1.set_output(PI4IOE1_LCD_RST, false)?;
    pi4ioe1.set_output(PI4IOE1_TP_RST, false)?;
    ms(100);
    pi4ioe1.set_output(PI4IOE1_LCD_RST, true)?;
    pi4ioe1.set_output(PI4IOE1_TP_RST, true)?;
    ms(20);
    gpio::set_level(TOUCH_PIN_INT, false)?;
    ms(100);
    gpio::set_direction(TOUCH_PIN_INT, gpio::Mode::Input)?;
    ms(50);
    Ok(())
}

fn panel_init(config: &Config, bus: &MasterBus) -> Result<Display> {
    let probe = Duration::from_millis(10);
    let pixel_format = resolve_pixel_format(config);
    let fb_num = resolve_fb_num(config);
    if bus.probe(st712x_touch::ADDR, probe).is_ok() {
        let version = st712x_version(bus);
        info!(TAG, "panel: ST712x (version: {version:?})");
        if version == Some(1) {
            let lcd = st7121::create(&st7121::Config {
                size: PANEL_SIZE,
                pixel_format,
                fb_num,
                backlight_gpio: Some(LCD_PIN_BL),
                backlight_min_duty: LCD_BL_MIN_DUTY,
            })
            .inspect_err(|err| error!(TAG, "st7121: {err}"))?;
            return Ok(Display::St7121(lcd));
        }
        let lcd = st7123::create(&st7123::Config {
            size: PANEL_SIZE,
            pixel_format,
            fb_num,
            backlight_gpio: Some(LCD_PIN_BL),
            backlight_min_duty: LCD_BL_MIN_DUTY,
        })
        .inspect_err(|err| error!(TAG, "st7123: {err}"))?;
        return Ok(Display::St7123(lcd));
    }
    if bus.probe(gt911::ADDR_PRIMARY, probe).is_ok() {
        info!(TAG, "panel: ILI9881C");
        let lcd = ili9881c::create(&ili9881c::Config {
            size: PANEL_SIZE,
            pixel_format,
            fb_num,
            backlight_gpio: Some(LCD_PIN_BL),
            backlight_min_duty: LCD_BL_MIN_DUTY,
        })
        .inspect_err(|err| error!(TAG, "ili9881c: {err}"))?;
        return Ok(Display::Ili9881c(lcd));
    }
    error!(TAG, "no known panel found on I2C");
    Err(Error::NOT_FOUND)
}

// Non-fatal: the board still comes up with the display alone.
fn touch_init(bus: &'static MasterBus, display: &Display) -> Option<&'static Touch> {
    let config = touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        int: touch::Int::Gpio(TOUCH_PIN_INT),
    };
    let result = match display {
        Display::Ili9881c(_) => Gt911::new(bus, &gt911::Config {
            address: gt911::ADDR_PRIMARY,
            clock_hz: gt911::DEFAULT_HZ,
            int_pin: Some(TOUCH_PIN_INT),
        })
        .and_then(|gt911| touch::start(gt911, config)),
        Display::St7121(_) | Display::St7123(_) => St712xTouch::new(bus, &st712x_touch::Config {
            clock_hz: st712x_touch::DEFAULT_HZ,
            int_pin: Some(TOUCH_PIN_INT),
        })
        .and_then(|st712x| touch::start(st712x, config)),
    };
    result.inspect_err(|err| warn!(TAG, "touch unavailable: {err}")).ok()
}

fn st712x_version(bus: &MasterBus) -> Option<u8> {
    let dev = bus
        .add_device(st712x_touch::ADDR, st712x_touch::DEFAULT_HZ)
        .inspect_err(|err| warn!(TAG, "attach ST712x for version read: {err}"))
        .ok()?;
    let mut version = [0u8];
    dev.transmit_receive(&[0x00, 0x00], &mut version, Duration::from_millis(50))
        .inspect_err(|err| warn!(TAG, "read ST712x version: {err}"))
        .ok()?;
    Some(version[0])
}
