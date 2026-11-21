/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::{error, info};
use idf::sync::OnceLock;

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

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

struct UnknownPower;

impl power::Driver for UnknownPower {}

pub(crate) fn init(config: &Config) -> Result<Board> {
    check_display(config)?;
    info!(TAG, "board: PaperS3");
    let panel = ed047tc1::create(ed047tc1::Config {
        data: EPD_PINS_DATA,
        sph: EPD_PIN_SPH,
        cl: EPD_PIN_CL,
        dc_dummy: EPD_CONTROL.pwr,
        control: EPD_CONTROL,
        ppi: PANEL_PPI,
        priority: EPD_PRIORITY,
        core: Some(EPD_CORE),
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
