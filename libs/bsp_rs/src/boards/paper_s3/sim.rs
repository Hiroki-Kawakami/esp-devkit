/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::{info, warn};
use idf::sync::OnceLock;

use super::{PANEL_PPI, PANEL_SIZE, PIXEL_FORMAT, TAG, check_display};
use crate::display::{self, Rotation, Rotations};
use crate::audio;
use crate::power;
use crate::rtc;
use crate::sd;
use crate::sim::audio::SimAudio;
use crate::sim::gram::{GramConfig, SimGram};
use crate::sim::imu as sim_imu;
use crate::sim::power::SimPower;
use crate::sim::rtc::SimRtc;
use crate::sim::sd::SimSd;
use crate::sim::{self, protocol::Event, touch as sim_touch};
use crate::touch;
use crate::{Board, Config};

pub(crate) type Display = SimGram;
// The buzzer, played as the tones a speaker sings.
pub(crate) type AudioDriver = SimAudio;

// The panel's memory is the chip's own, so writes land as fast as it copies;
// the host shows that memory, not the drives.
const PANEL: GramConfig = GramConfig {
    size: PANEL_SIZE,
    ppi: PANEL_PPI,
    format: PIXEL_FORMAT,
    alignment: 1,
    rotations: Rotations::R0,
    bytes_per_second: u32::MAX,
    epd: true,
};

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// Without the simulator host there is nothing to show or touch.
pub(crate) fn init(config: &Config) -> Result<Board> {
    check_display(config)?;
    let Some(host) = sim::host() else {
        info!(TAG, "board: PaperS3 (simulator, no host)");
        let power = power::start(SimPower::new(None), None);
        return Ok(Board { audio: None, buttons: None, display: None, imu: None, power, rtc: None, sd: None, touch: None });
    };
    info!(TAG, "board: PaperS3 (simulator)");
    host.send(&Event::device("M5Stack PaperS3"));
    let panel = SimGram::new(host, &PANEL, Rotation::R0)?;
    let touch = sim_touch::start(host, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        rotation: Rotation::R0,
        int: touch::Int::External,
        zones: &[],
    })
    .inspect_err(|err| warn!(TAG, "touch unavailable: {err}"))
    .ok();
    let audio = SimAudio::buzzer(host)
        .and_then(|driver| audio::start(driver, &super::buzzer_config(config)))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok();
    let power = power::start(SimPower::new(Some(host)), audio);
    let imu = sim_imu::start(host).inspect_err(|err| warn!(TAG, "imu unavailable: {err}")).ok();
    let rtc = SimRtc::new(host).and_then(|driver| rtc::start(driver).inspect_err(|err| warn!(TAG, "rtc unavailable: {err}")).ok());
    let sd = sd::start(SimSd::new()).inspect_err(|err| warn!(TAG, "sd unavailable: {err}")).ok();
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
