/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::Result;
use idf::log::{info, warn};
use idf::sync::OnceLock;

use super::{PANEL_SIZE, TAG, resolve_fb_num, resolve_pixel_format};
use crate::audio;
use crate::display;
use crate::sim::audio::SimAudio;
use crate::sim::panel::SimPanel;
use crate::sim::protocol::Event;
use crate::sim::{self, touch as sim_touch};
use crate::touch;
use crate::{Board, Config};

pub(crate) type AudioDriver = SimAudio;
pub(crate) type Display = SimPanel;

static DISPLAY: OnceLock<display::Display> = OnceLock::new();

// Without the simulator host there is nothing to show or touch.
pub(crate) fn init(config: &Config) -> Result<Board> {
    let Some(host) = sim::host() else {
        info!(TAG, "board: Tab5 (simulator, no host)");
        return Ok(Board { audio: None, display: None, touch: None });
    };
    info!(TAG, "board: Tab5 (simulator)");
    host.send(&Event::device("M5Stack Tab5"));
    let panel = SimPanel::new(host, PANEL_SIZE, resolve_pixel_format(config), resolve_fb_num(config))?;
    let touch = sim_touch::start(host, touch::Config {
        width: PANEL_SIZE.width,
        height: PANEL_SIZE.height,
        swap_xy: false,
        mirror_x: false,
        mirror_y: false,
        int: touch::Int::External,
    })
    .inspect_err(|err| warn!(TAG, "touch unavailable: {err}"))
    .ok();
    let audio = SimAudio::new(host)
        .and_then(|driver| audio::start(driver, &config.audio))
        .inspect_err(|err| warn!(TAG, "audio unavailable: {err}"))
        .ok();
    Ok(Board { audio, display: Some(DISPLAY.get_or_init(|| display::Display(panel))), touch })
}
