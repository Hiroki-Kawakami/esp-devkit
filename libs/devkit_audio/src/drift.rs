/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

const DEFAULT_KP: f32 = 100.0;
const DEFAULT_KI: f32 = 3.0;
const DEFAULT_MAX_PPM: f32 = 1000.0;
const DEFAULT_SMOOTHING_MS: u32 = 500;

#[derive(Clone, Copy, Debug, Default)]
pub struct DriftConfig {
    /// The rate the level is counted in.
    pub sample_rate: u32,
    pub target_frames: usize,
    /// ppm per ms of level error; 0 = 100.
    pub kp: f32,
    /// ppm per ms of level error per second; 0 = 3.
    pub ki: f32,
    /// 0 = 1000.
    pub max_ppm: f32,
    /// Level low-pass time constant; 0 = 500.
    pub smoothing_ms: u32,
}

/// PI controller from a buffer fill level to a resampler adjustment in ppm.
#[derive(Clone, Copy, Debug)]
pub struct Drift {
    config: DriftConfig,
    level_ms: f32,
    integral: f32,
    ppm: f32,
    started: bool,
}

fn or_default(value: f32, default: f32) -> f32 {
    if value == 0.0 { default } else { value }
}

impl Drift {
    pub fn new(config: &DriftConfig) -> Self {
        let config = DriftConfig {
            kp: or_default(config.kp, DEFAULT_KP),
            ki: or_default(config.ki, DEFAULT_KI),
            max_ppm: or_default(config.max_ppm, DEFAULT_MAX_PPM),
            smoothing_ms: if config.smoothing_ms == 0 { DEFAULT_SMOOTHING_MS } else { config.smoothing_ms },
            ..*config
        };
        Self { config, level_ms: 0.0, integral: 0.0, ppm: 0.0, started: false }
    }

    pub fn reset(&mut self) {
        self.level_ms = 0.0;
        self.integral = 0.0;
        self.ppm = 0.0;
        self.started = false;
    }

    pub fn ppm(&self) -> f32 {
        self.ppm
    }

    /// `elapsed_frames`: frames the level's consumer took since the last update.
    pub fn update(&mut self, level: usize, elapsed_frames: usize) -> f32 {
        let c = &self.config;
        let ms_per_frame = 1000.0 / c.sample_rate as f32;
        let level_ms = level as f32 * ms_per_frame;
        let elapsed_ms = elapsed_frames as f32 * ms_per_frame;
        if self.started {
            let alpha = elapsed_ms / (c.smoothing_ms as f32 + elapsed_ms);
            self.level_ms += alpha * (level_ms - self.level_ms);
        } else {
            self.level_ms = level_ms;
            self.started = true;
        }
        let error_ms = self.level_ms - c.target_frames as f32 * ms_per_frame;
        self.integral = (self.integral + c.ki * error_ms * elapsed_ms * 1e-3).clamp(-c.max_ppm, c.max_ppm);
        self.ppm = (c.kp * error_ms + self.integral).clamp(-c.max_ppm, c.max_ppm);
        self.ppm
    }
}
