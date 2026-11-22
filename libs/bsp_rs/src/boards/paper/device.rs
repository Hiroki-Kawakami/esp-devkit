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
use idf::{Result, gpio, spi, thread};

use super::power::PowerDriver;
use super::{PANEL_PPI, PANEL_SIZE, TAG, resolve_display};
use crate::button::{self, Buttons, Provider};
use crate::devices::bm8563::{self, Bm8563};
use crate::devices::gt911::{self, Gt911};
use crate::devices::it8951e::{self, It8951e};
use crate::display::{self, Rotation};
use crate::driver::gpio_button::{GpioButtons, Pin};
use crate::driver::sd_spi::{self, SdSpi};
use crate::power;
use crate::rtc::{self, Rtc};
use crate::sd;
use crate::touch::{self, Touch};
use crate::{Absent, Board, Config};

pub(crate) type Display = It8951e;
pub(crate) type AudioDriver = Absent;

// Holds the main rail up on battery once the power button is let go.
const PIN_MAIN_PWR: i32 = 2;
const PIN_EXT_PWR_EN: i32 = 5;
// The controller has no reset line, so its supply is cycled instead.
const PIN_EPD_PWR_EN: i32 = 23;
const EPD_OFF: Duration = Duration::from_millis(2);
const EPD_BOOT: Duration = Duration::from_millis(10);

// Shared by the panel controller and the SD card.
const SPI_HOST: i32 = 2;
const SPI_PIN_SCK: i32 = 14;
const SPI_PIN_MOSI: i32 = 12;
const SPI_PIN_MISO: i32 = 13;
const SPI_MAX_TRANSFER: usize = 4096;

const SD_PIN_CS: i32 = 4;
// Unstable at 40 MHz.
const SD_FREQ_KHZ: i32 = 20_000;

const EPD_PIN_CS: i32 = 15;
const EPD_PIN_HRDY: i32 = 27;
// Reads above 10 MHz come back wrong at any input delay.
const EPD_WRITE_HZ: u32 = 40_000_000;
const EPD_READ_HZ: u32 = 10_000_000;
const EPD_INPUT_DELAY_NS: i32 = 0;
const EPD_VCOM_MV: u16 = 2300;
const EPD_FALLBACK_ADDRESS: u32 = 0x0012_36E0;
const EPD_REST_AFTER: Duration = Duration::from_secs(3);

// Shared by the touch and the RTC.
const I2C_PORT: i32 = 1;
const I2C_PIN_SDA: i32 = 21;
const I2C_PIN_SCL: i32 = 22;
// An input-only pad, without a pull of its own.
const TOUCH_PIN_INT: i32 = 36;

// The wheel's push, up and down: input-only pads pulled up on the board.
static KEYS: [Pin; 3] = [
    Pin { gpio: 38, active_low: true, pull: false },
    Pin { gpio: 37, active_low: true, pull: false },
    Pin { gpio: 39, active_low: true, pull: false },
];

static I2C: OnceLock<MasterBus> = OnceLock::new();
static DISPLAY: OnceLock<display::Display> = OnceLock::new();

pub(crate) fn init(config: &Config) -> Result<Board> {
    let (pixel_format, rotation) = resolve_display(config)?;
    info!(TAG, "board: Paper");
    power_on().inspect_err(|err| error!(TAG, "power rails: {err}"))?;
    let bus = spi::Bus::new(&spi::BusConfig {
        host: SPI_HOST,
        sclk: SPI_PIN_SCK,
        mosi: SPI_PIN_MOSI,
        miso: Some(SPI_PIN_MISO),
        quad: None,
        max_transfer_bytes: SPI_MAX_TRANSFER,
    })
    .inspect_err(|err| error!(TAG, "spi bus: {err}"))?;
    let sd = SdSpi::new(sd_spi::Config { bus: bus.clone(), cs: SD_PIN_CS, max_freq_khz: SD_FREQ_KHZ })
        .inspect_err(|err| warn!(TAG, "sd unavailable: {err}"))
        .ok();
    let panel = It8951e::new(it8951e::Config {
        bus: bus.clone(),
        cs: EPD_PIN_CS,
        hrdy: EPD_PIN_HRDY,
        write_hz: EPD_WRITE_HZ,
        read_hz: EPD_READ_HZ,
        input_delay_ns: EPD_INPUT_DELAY_NS,
        size: PANEL_SIZE,
        ppi: PANEL_PPI,
        vcom_mv: EPD_VCOM_MV,
        fallback_address: EPD_FALLBACK_ADDRESS,
        pixel_format,
        rotation,
        rest_after: EPD_REST_AFTER,
    })
    .inspect_err(|err| error!(TAG, "panel: {err}"))?;
    let bus = I2C
        .get_or_try_init(|| MasterBus::new(&BusConfig { port: I2C_PORT, sda: I2C_PIN_SDA, scl: I2C_PIN_SCL, glitch_ignore_cnt: 7, internal_pullup: true }))
        .inspect_err(|err| error!(TAG, "i2c bus: {err}"))
        .ok();
    let touch = bus.and_then(|bus| touch_init(bus, rotation).inspect_err(|err| warn!(TAG, "touch unavailable: {err}")).ok());
    let rtc = bus.and_then(rtc_init);
    let buttons = buttons_init().inspect_err(|err| warn!(TAG, "buttons unavailable: {err}")).ok();
    let power = power::start(PowerDriver::new(PIN_MAIN_PWR), None);
    let sd = sd.and_then(|sd| sd::start(sd).inspect_err(|err| warn!(TAG, "sd unavailable: {err}")).ok());
    Ok(Board {
        audio: None,
        buttons,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(panel))),
        imu: None,
        power,
        rtc,
        sd,
        touch,
    })
}

// Its INT turns the power on, and holds it while the timer flag stays set.
fn rtc_init(bus: &'static MasterBus) -> Option<&'static Rtc> {
    let rtc = Bm8563::new(bus, bm8563::ADDR).and_then(rtc::start).inspect_err(|err| warn!(TAG, "rtc unavailable: {err}")).ok()?;
    if let Err(err) = rtc.stop_timer() {
        error!(TAG, "rtc timer stop: {err}");
    }
    Some(rtc)
}

fn buttons_init() -> Result<&'static Buttons> {
    button::start(vec![Provider::Level(Box::new(GpioButtons::new(&KEYS)?))])
}

// The chip reports the panel's portrait axes with Y mirrored, and its reset
// is not on a GPIO, so either address may answer.
fn touch_init(bus: &'static MasterBus, rotation: Rotation) -> Result<&'static Touch> {
    let gt911 = Gt911::new(bus, &gt911::Config {
        address: gt911::ADDR_SECONDARY,
        clock_hz: gt911::DEFAULT_HZ,
        int_pin: Some(TOUCH_PIN_INT),
        int_pull_up: false,
    })?;
    touch::start(gt911, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: true,
        mirror_x: false,
        mirror_y: true,
        rotation,
        int: touch::Int::Gpio(TOUCH_PIN_INT),
        zones: &[],
    })
}

fn power_on() -> Result<()> {
    gpio::configure(&[PIN_MAIN_PWR, PIN_EXT_PWR_EN, PIN_EPD_PWR_EN], gpio::Mode::Output)?;
    gpio::set_level(PIN_MAIN_PWR, true)?;
    gpio::set_level(PIN_EXT_PWR_EN, true)?;
    gpio::set_level(PIN_EPD_PWR_EN, false)?;
    thread::sleep(EPD_OFF);
    gpio::set_level(PIN_EPD_PWR_EN, true)?;
    thread::sleep(EPD_BOOT);
    Ok(())
}
