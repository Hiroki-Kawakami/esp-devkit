/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec;

use idf::Result;
use idf::log::{info, warn};
use idf::sync::OnceLock;

use super::{PANEL_PPI, PANEL_SIZE, PIXEL_FORMAT, TAG, resolve_rotation};
use crate::audio;
use crate::button::{self, Provider};
use crate::display;
use crate::power;
use crate::rtc;
use crate::sim::audio::SimAudio;
use crate::sim::gram::{GramConfig, SimGram};
use crate::sim::power::SimPower;
use crate::sim::rtc::SimRtc;
use crate::sim::{self, protocol::Event, touch as sim_touch};
use crate::touch;
use crate::{Board, Config};

pub(crate) type AudioDriver = SimAudio;
pub(crate) type Display = SimGram;

// The CO5300, on four lines of its 80 MHz QSPI clock.
const PANEL: GramConfig = GramConfig {
    size: PANEL_SIZE,
    ppi: PANEL_PPI,
    format: PIXEL_FORMAT,
    alignment: 2,
    quarter_turns: false,
    bytes_per_second: 80_000_000 / 2,
};

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// KEYA, KEYB and the power button, which only a harness presses.
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
    let rotation = resolve_rotation(config)?;
    let Some(host) = sim::host() else {
        info!(TAG, "board: StopWatch (simulator, no host)");
        let power = power::start(SimPower::new(None), None);
        return Ok(Board { audio: None, buttons: None, display: None, imu: None, power, rtc: None, sd: None, touch: None });
    };
    info!(TAG, "board: StopWatch (simulator)");
    host.send(&Event::device("M5Stack StopWatch"));
    let panel = SimGram::new(host, &PANEL, rotation)?;
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
    let audio = SimAudio::new(host)
        .and_then(|driver| audio::start(driver, &config.audio))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok();
    let power = power::start(SimPower::new(Some(host)), audio);
    let rtc = SimRtc::new(host).and_then(|driver| rtc::start(driver).inspect_err(|err| warn!(TAG, "rtc unavailable: {err}")).ok());
    Ok(Board {
        audio,
        buttons,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(panel))),
        imu: None,
        power,
        rtc,
        sd: None,
        touch,
    })
}
