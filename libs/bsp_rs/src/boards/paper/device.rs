/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::time::Duration;

use idf::log::{error, info};
use idf::sync::OnceLock;
use idf::{Result, gpio, spi, thread};

use super::{PANEL_PPI, PANEL_SIZE, TAG, resolve_display};
use crate::devices::it8951e::{self, It8951e};
use crate::display;
use crate::power;
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

const EPD_PIN_CS: i32 = 15;
const EPD_PIN_HRDY: i32 = 27;
// Reads above 10 MHz come back wrong at any input delay.
const EPD_WRITE_HZ: u32 = 40_000_000;
const EPD_READ_HZ: u32 = 10_000_000;
const EPD_INPUT_DELAY_NS: i32 = 0;
const EPD_VCOM_MV: u16 = 2300;
const EPD_FALLBACK_ADDRESS: u32 = 0x0012_36E0;
const EPD_REST_AFTER: Duration = Duration::from_secs(3);

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

struct UnknownPower;

impl power::Driver for UnknownPower {}

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
    let power = power::start(UnknownPower, None);
    Ok(Board {
        audio: None,
        buttons: None,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(panel))),
        imu: None,
        power,
        rtc: None,
        sd: None,
        touch: None,
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
