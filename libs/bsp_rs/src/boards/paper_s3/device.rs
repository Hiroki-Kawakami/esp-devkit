/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::time::Duration;

use idf::i2c::{BusConfig, MasterBus};
use idf::log::{error, info, warn};
use idf::sync::OnceLock;
use idf::time::delay_us;
use idf::{Result, gpio, spi};

use super::power::PowerDriver;
use super::t5::{self, ShiftControl};
use super::{PANEL_PPI, PANEL_SIZE, TAG, check_display};
use crate::audio::{self, Audio};
use crate::devices::bm8563::{self, Bm8563};
use crate::devices::bmi270::{self, Bmi270};
use crate::devices::ed047tc1;
use crate::devices::gt911::{self, Gt911};
use crate::display::{self, Rotation};
use crate::driver::epd_ll::{ControlPins, EpdLl};
use crate::driver::pwm_buzzer::{self, PwmBuzzer};
use crate::driver::sd_spi::{self, SdSpi};
use crate::imu::{self, AxisMap, Imu};
use crate::power;
use crate::rtc::{self, Rtc};
use crate::sd::{self, Sd};
use crate::touch::{self, Touch};
use crate::{Board, Config};

pub(crate) type Display = EpdLl;
pub(crate) type AudioDriver = PwmBuzzer;

const EPD_PINS_DATA: [i32; 8] = [6, 14, 7, 12, 9, 11, 8, 10];
const EPD_PIN_SPH: i32 = 13;
const EPD_PIN_CL: i32 = 16;
const EPD_CONTROL: ControlPins = ControlPins { ckv: 18, spv: 17, le: 15, oe: 45, pwr: 46 };
const EPD_PRIORITY: u32 = 5;
// Keeps its waits for the bus off the core apps draw on.
const EPD_CORE: u32 = 0;

// Shared by the touch and the RTC.
const I2C_PORT: i32 = 1;
const I2C_PIN_SDA: i32 = 41;
const I2C_PIN_SCL: i32 = 42;
const TOUCH_PIN_INT: i32 = 48;

const BUZZER_PIN: i32 = 21;
const BUZZER_TIMER: u32 = 0;
const BUZZER_CHANNEL: u32 = 0;

// The chip lies turned half a turn in the panel's plane.
const IMU_AXIS_MAP: AxisMap = AxisMap { x: -1, y: -2, z: 3 };

const SD_SPI_HOST: i32 = 1;
const SD_PIN_SCK: i32 = 39;
const SD_PIN_MOSI: i32 = 38;
const SD_PIN_MISO: i32 = 40;
const SD_PIN_CS: i32 = 47;
const SD_FREQ_KHZ: i32 = 20_000;
const SD_MAX_TRANSFER: usize = 4000;

// The PaperS3 pulls its I2C SDA up for good; on the T5 the pin is the
// panel's CL, which nothing pulls.
const PROBE_PIN: i32 = 41;
const PROBE_SETTLE_US: u32 = 100;
const PROBE_TIMEOUT: Duration = Duration::from_millis(50);

/// The board's I2C, whichever board it is.
static I2C: OnceLock<MasterBus> = OnceLock::new();
static DISPLAY: OnceLock<display::Display> = OnceLock::new();

#[derive(Clone, Copy)]
enum Model {
    PaperS3,
    T5,
}

impl Model {
    fn i2c(self) -> BusConfig {
        let (port, sda, scl) = match self {
            Self::PaperS3 => (I2C_PORT, I2C_PIN_SDA, I2C_PIN_SCL),
            Self::T5 => (t5::I2C_PORT, t5::I2C_PIN_SDA, t5::I2C_PIN_SCL),
        };
        BusConfig { port, sda, scl, glitch_ignore_cnt: 7, internal_pullup: true }
    }

    fn touch_int(self) -> i32 {
        match self {
            Self::PaperS3 => TOUCH_PIN_INT,
            Self::T5 => t5::PIN_TOUCH_INT,
        }
    }
}

/// Reads the pins as inputs and talks only on the T5's own I2C, so neither
/// panel is driven before the board is known. The T5's bus stays open.
fn detect() -> Model {
    let pulled_up = gpio::set_direction(PROBE_PIN, gpio::Mode::Input).and_then(|()| gpio::set_pull(PROBE_PIN, gpio::Pull::Down)).is_ok() && {
        delay_us(PROBE_SETTLE_US);
        gpio::level(PROBE_PIN)
    };
    let _ = gpio::set_pull(PROBE_PIN, gpio::Pull::None);
    if pulled_up {
        return Model::PaperS3;
    }
    if let Ok(bus) = MasterBus::new(&Model::T5.i2c())
        && bus.probe(t5::RTC_ADDR, PROBE_TIMEOUT).is_ok()
    {
        let _ = I2C.set(bus);
        return Model::T5;
    }
    panic!("board: neither a PaperS3 (GPIO{PROBE_PIN} not pulled up) nor a LilyGo T5 (no RTC at 0x{:02x})", t5::RTC_ADDR);
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
    let bus = i2c_init(model).ok();
    let touch = bus.and_then(|bus| touch_init(bus, model).inspect_err(|err| warn!(TAG, "touch unavailable: {err}")).ok());
    let rtc = bus.and_then(rtc_init);
    let sd = sd_init(model);
    let imu = match model {
        Model::PaperS3 => bus.and_then(imu_init),
        Model::T5 => None,
    };
    let audio = match model {
        Model::PaperS3 => audio_init(config),
        Model::T5 => None,
    };
    let power = power::start(
        match model {
            Model::PaperS3 => PowerDriver::paper_s3(),
            Model::T5 => PowerDriver::t5(),
        },
        audio,
    );
    Ok(Board {
        audio,
        buttons: None,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(panel))),
        imu,
        power,
        rtc,
        sd,
        touch,
    })
}

fn audio_init(config: &Config) -> Option<&'static Audio> {
    PwmBuzzer::new(&pwm_buzzer::Config { gpio: BUZZER_PIN, timer: BUZZER_TIMER, channel: BUZZER_CHANNEL })
        .and_then(|buzzer| audio::start(buzzer, &super::buzzer_config(config)))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok()
}

fn i2c_init(model: Model) -> Result<&'static MasterBus> {
    I2C.get_or_try_init(|| MasterBus::new(&model.i2c())).inspect_err(|err| error!(TAG, "i2c bus: {err}"))
}

// A BM8563 on the PaperS3, whose INT reaches the power controller; a PCF8563,
// the same registers, on the T5.
fn sd_init(model: Model) -> Option<&'static Sd> {
    let (sclk, mosi, miso, cs) = match model {
        Model::PaperS3 => (SD_PIN_SCK, SD_PIN_MOSI, SD_PIN_MISO, SD_PIN_CS),
        Model::T5 => (t5::SD_PIN_SCK, t5::SD_PIN_MOSI, t5::SD_PIN_MISO, t5::SD_PIN_CS),
    };
    spi::Bus::new(&spi::BusConfig { host: SD_SPI_HOST, sclk, mosi, miso: Some(miso), quad: None, max_transfer_bytes: SD_MAX_TRANSFER })
        .and_then(|bus| sd::start(SdSpi::new(sd_spi::Config { bus, cs, max_freq_khz: SD_FREQ_KHZ })))
        .inspect_err(|err| warn!(TAG, "sd unavailable: {err}"))
        .ok()
}

// INT1 switches the power controller's trigger, as the off pulse does: left
// at its reset state, output disabled, the chip never drives it.
fn imu_init(bus: &'static MasterBus) -> Option<&'static Imu> {
    Bmi270::new(bus, bmi270::ADDR, bmi270::DEFAULT_HZ)
        .and_then(|chip| imu::start(chip, IMU_AXIS_MAP))
        .inspect_err(|err| warn!(TAG, "imu unavailable: {err}"))
        .ok()
}

fn rtc_init(bus: &'static MasterBus) -> Option<&'static Rtc> {
    let rtc = Bm8563::new(bus, bm8563::ADDR).and_then(rtc::start).inspect_err(|err| warn!(TAG, "rtc unavailable: {err}")).ok()?;
    if let Err(err) = rtc.stop_timer() {
        error!(TAG, "rtc timer stop: {err}");
    }
    Some(rtc)
}

// On both boards the chip reports the panel's portrait axes, its Y mirrored,
// and its reset is not on a GPIO, so either address may answer.
fn touch_init(bus: &'static MasterBus, model: Model) -> Result<&'static Touch> {
    let int = model.touch_int();
    let gt911 = Gt911::new(bus, &gt911::Config { address: gt911::ADDR_PRIMARY, clock_hz: gt911::DEFAULT_HZ, int_pin: Some(int) })?;
    touch::start(gt911, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: true,
        mirror_x: false,
        mirror_y: true,
        rotation: Rotation::R0,
        int: touch::Int::Gpio(int),
        zones: &[],
    })
}
