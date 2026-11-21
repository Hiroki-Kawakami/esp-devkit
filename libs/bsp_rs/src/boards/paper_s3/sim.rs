/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::info;
use idf::sync::OnceLock;

use super::{PANEL_PPI, PIXEL_FORMAT, TAG, check_display};
use crate::display::{self, Rotations, Size};
use crate::power;
use crate::sim::gram::{GramConfig, SimGram};
use crate::sim::power::SimPower;
use crate::sim::{self, protocol::Event};
use crate::{Board, Config};

pub(crate) type Display = SimGram;

// The panel's memory is the chip's own, so writes land as fast as it copies;
// the host shows that memory, not the drives.
const PANEL: GramConfig = GramConfig {
    size: Size { width: 960, height: 540 },
    ppi: PANEL_PPI,
    format: PIXEL_FORMAT,
    alignment: 1,
    rotations: Rotations::R0,
    bytes_per_second: u32::MAX,
    epd: true,
};

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// Without the simulator host there is nothing to show.
pub(crate) fn init(config: &Config) -> Result<Board> {
    check_display(config)?;
    let Some(host) = sim::host() else {
        info!(TAG, "board: PaperS3 (simulator, no host)");
        let power = power::start(SimPower::new(None), None);
        return Ok(Board { audio: None, buttons: None, display: None, imu: None, power, rtc: None, sd: None, touch: None });
    };
    info!(TAG, "board: PaperS3 (simulator)");
    host.send(&Event::device("M5Stack PaperS3"));
    let panel = SimGram::new(host, &PANEL, display::Rotation::R0)?;
    let power = power::start(SimPower::new(Some(host)), None);
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
