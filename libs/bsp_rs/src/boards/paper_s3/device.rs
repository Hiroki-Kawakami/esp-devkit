/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::{error, info};
use idf::sync::OnceLock;
use idf::time::delay_us;
use idf::{Result, gpio};

use super::t5::{self, ShiftControl};
use super::{PANEL_PPI, TAG, check_display};
use crate::devices::ed047tc1;
use crate::display;
use crate::driver::epd_ll::{ControlPins, EpdLl};
use crate::power;
use crate::{Board, Config};

pub(crate) type Display = EpdLl;

const EPD_PINS_DATA: [i32; 8] = [6, 14, 7, 12, 9, 11, 8, 10];
const EPD_PIN_SPH: i32 = 13;
const EPD_PIN_CL: i32 = 16;
const EPD_CONTROL: ControlPins = ControlPins { ckv: 18, spv: 17, le: 15, oe: 45, pwr: 46 };
const EPD_PRIORITY: u32 = 5;
// Keeps its waits for the bus off the core apps draw on.
const EPD_CORE: u32 = 0;

// The PaperS3 pulls its I2C SDA up for good; on the T5 the pin is the
// panel's CL, which nothing pulls.
const PROBE_PIN: i32 = 41;
const PROBE_SETTLE_US: u32 = 100;
const T5_I2C_PORT: i32 = 0;
const T5_I2C_PIN_SDA: i32 = 18;
const T5_I2C_PIN_SCL: i32 = 17;
const T5_RTC_ADDR: u16 = 0x51;
const T5_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

#[derive(Clone, Copy)]
enum Model {
    PaperS3,
    T5,
}

struct UnknownPower;

impl power::Driver for UnknownPower {}

/// Reads the pins as inputs and talks only on the T5's own I2C, so neither
/// panel is driven before the board is known.
fn detect() -> Model {
    let pulled_up = gpio::set_direction(PROBE_PIN, gpio::Mode::Input).and_then(|()| gpio::set_pull(PROBE_PIN, gpio::Pull::Down)).is_ok() && {
        delay_us(PROBE_SETTLE_US);
        gpio::level(PROBE_PIN)
    };
    let _ = gpio::set_pull(PROBE_PIN, gpio::Pull::None);
    if pulled_up {
        return Model::PaperS3;
    }
    let rtc = MasterBus::new(&BusConfig {
        port: T5_I2C_PORT,
        sda: T5_I2C_PIN_SDA,
        scl: T5_I2C_PIN_SCL,
        glitch_ignore_cnt: 7,
        internal_pullup: true,
    })
    .and_then(|bus| bus.probe(T5_RTC_ADDR, T5_PROBE_TIMEOUT));
    if rtc.is_ok() {
        return Model::T5;
    }
    panic!("board: neither a PaperS3 (GPIO{PROBE_PIN} not pulled up) nor a LilyGo T5 (no RTC at 0x{T5_RTC_ADDR:02x})");
}

pub(crate) fn init(config: &Config) -> Result<Board> {
    check_display(config)?;
    let model = detect();
    let panel = match model {
        Model::PaperS3 => {
            info!(TAG, "board: PaperS3");
            ed047tc1::Config {
                data: EPD_PINS_DATA,
                sph: EPD_PIN_SPH,
                cl: EPD_PIN_CL,
                dc_dummy: EPD_CONTROL.pwr,
                control: Box::new(EPD_CONTROL),
                ppi: PANEL_PPI,
                priority: EPD_PRIORITY,
                core: Some(EPD_CORE),
            }
        }
        Model::T5 => {
            info!(TAG, "board: LilyGo T5");
            ed047tc1::Config {
                data: t5::PINS_DATA,
                sph: t5::PIN_SPH,
                cl: t5::PIN_CL,
                dc_dummy: t5::PIN_DC_DUMMY,
                control: Box::new(ShiftControl::new()),
                ppi: PANEL_PPI,
                priority: EPD_PRIORITY,
                core: Some(EPD_CORE),
            }
        }
    };
    let panel = ed047tc1::create(panel).inspect_err(|err| error!(TAG, "panel: {err}"))?;
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
