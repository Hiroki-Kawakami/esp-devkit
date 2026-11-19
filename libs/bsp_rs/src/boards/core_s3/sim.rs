/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::{info, warn};
use idf::sync::OnceLock;

use super::{PANEL_PPI, PANEL_SIZE, PIXEL_FORMAT, TAG, resolve_rotation};
use crate::audio;
use crate::display;
use crate::power;
use crate::rtc;
use crate::sim::audio::SimAudio;
use crate::sim::power::SimPower;
use crate::sim::rtc::SimRtc;
use crate::sim::{self, gram::SimGram, protocol::Event, touch as sim_touch};
use crate::touch;
use crate::{Board, Config};

pub(crate) type AudioDriver = SimAudio;
pub(crate) type Display = SimGram;

// The ILI9342C's 40 MHz SPI clock.
const PANEL_BYTES_PER_SECOND: u32 = 40_000_000 / 8;

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// Without the simulator host there is nothing to show or touch.
pub(crate) fn init(config: &Config) -> Result<Board> {
    let rotation = resolve_rotation(config)?;
    let Some(host) = sim::host() else {
        info!(TAG, "board: CoreS3 (simulator, no host)");
        let power = power::start(SimPower::new(None), None);
        return Ok(Board { audio: None, buttons: None, display: None, imu: None, power, rtc: None, sd: None, touch: None });
    };
    info!(TAG, "board: CoreS3 (simulator)");
    host.send(&Event::device("M5Stack CoreS3"));
    let panel = SimGram::new(host, PANEL_SIZE, PANEL_PPI, PIXEL_FORMAT, rotation, PANEL_BYTES_PER_SECOND)?;
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
    let audio = SimAudio::new(host)
        .and_then(|driver| audio::start(driver, &config.audio))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok();
    let power = power::start(SimPower::new(Some(host)), audio);
    let rtc = SimRtc::new(host).and_then(|driver| rtc::start(driver).inspect_err(|err| warn!(TAG, "rtc unavailable: {err}")).ok());
    Ok(Board {
        audio,
        buttons: None,
        display: Some(DISPLAY.get_or_init(|| display::Display::new(panel))),
        imu: None,
        power,
        rtc,
        sd: None,
        touch,
    })
}
