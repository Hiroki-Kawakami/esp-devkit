/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec;

use idf::Result;
use idf::log::{info, warn};
use idf::sync::OnceLock;

use super::{PANEL_PPI, PANEL_SIZE, TAG, resolve_display};
use crate::button::{self, Provider};
use crate::display::{self, Rotations};
use crate::power;
use crate::sim::gram::{GramConfig, SimGram};
use crate::sim::power::SimPower;
use crate::sim::{self, protocol::Event, touch as sim_touch};
use crate::touch;
use crate::{Absent, Board, Config};

pub(crate) type Display = SimGram;
pub(crate) type AudioDriver = Absent;

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// The wheel's push, up and down, which only a harness presses.
struct Keys;

impl button::Driver for Keys {
    fn count(&self) -> usize {
        3
    }

    fn sample(&mut self, pressed: &mut [bool]) -> Result<()> {
        pressed.fill(false);
        Ok(())
    }

    fn notifies(&self) -> bool {
        true
    }
}

// Without the simulator host there is nothing to show or touch.
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
    let touch = sim_touch::start(host, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        rotation,
        int: touch::Int::External,
        zones: &[],
    })
    .inspect_err(|err| warn!(TAG, "touch unavailable: {err}"))
    .ok();
    let buttons = button::start(vec![Provider::Level(Box::new(Keys))]).inspect_err(|err| warn!(TAG, "buttons unavailable: {err}")).ok();
    let power = power::start(SimPower::new(Some(host)), None);
    Ok(Board {
        audio: None,
        buttons,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(panel))),
        imu: None,
        power,
        rtc: None,
        sd: None,
        touch,
    })
}
