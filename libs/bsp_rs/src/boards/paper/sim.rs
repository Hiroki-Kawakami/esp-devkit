/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::info;
use idf::sync::OnceLock;

use super::{PANEL_PPI, PANEL_SIZE, TAG, resolve_display};
use crate::display::{self, Rotations};
use crate::power;
use crate::sim::gram::{GramConfig, SimGram};
use crate::sim::power::SimPower;
use crate::sim::{self, protocol::Event};
use crate::{Absent, Board, Config};

pub(crate) type Display = SimGram;
pub(crate) type AudioDriver = Absent;

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// Without the simulator host there is nothing to show.
pub(crate) fn init(config: &Config) -> Result<Board> {
    let (format, rotation) = resolve_display(config)?;
    let Some(host) = sim::host() else {
        info!(TAG, "board: Paper (simulator, no host)");
        let power = power::start(SimPower::new(None), None);
        return Ok(Board { audio: None, buttons: None, display: None, imu: None, power, rtc: None, sd: None, touch: None });
    };
    info!(TAG, "board: Paper (simulator)");
    host.send(&Event::device("M5Stack Paper"));
    // The panel's memory is the chip's own, so writes land as fast as it
    // copies; the host shows that memory, not the drives.
    let panel = GramConfig {
        size: PANEL_SIZE,
        ppi: PANEL_PPI,
        format,
        alignment: 2,
        rotations: Rotations::all(),
        bytes_per_second: u32::MAX,
        epd: true,
    };
    let panel = SimGram::new(host, &panel, rotation)?;
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
