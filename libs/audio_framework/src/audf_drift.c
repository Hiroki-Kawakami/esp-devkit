/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_drift.h"

#define DEFAULT_KP           100.0f
#define DEFAULT_KI           3.0f
#define DEFAULT_MAX_PPM      1000.0f
#define DEFAULT_SMOOTHING_MS 500

void audf_drift_init(audf_drift_t *drift, const audf_drift_config_t *config) {
    drift->config = *config;
    if (!drift->config.kp) drift->config.kp = DEFAULT_KP;
    if (!drift->config.ki) drift->config.ki = DEFAULT_KI;
    if (!drift->config.max_ppm) drift->config.max_ppm = DEFAULT_MAX_PPM;
    if (!drift->config.smoothing_ms) drift->config.smoothing_ms = DEFAULT_SMOOTHING_MS;
    audf_drift_reset(drift);
}

void audf_drift_reset(audf_drift_t *drift) {
    drift->level_ms = 0.0f;
    drift->integral = 0.0f;
    drift->ppm = 0.0f;
    drift->started = false;
}

static float clamp(float v, float limit) {
    if (v > limit) return limit;
    if (v < -limit) return -limit;
    return v;
}

float audf_drift_update(audf_drift_t *drift, size_t level, size_t elapsed_frames) {
    const audf_drift_config_t *c = &drift->config;
    float ms_per_frame = 1000.0f / (float)c->sample_rate;
    float level_ms = (float)level * ms_per_frame;
    float elapsed_ms = (float)elapsed_frames * ms_per_frame;
    if (!drift->started) {
        drift->level_ms = level_ms;
        drift->started = true;
    } else {
        float alpha = elapsed_ms / ((float)c->smoothing_ms + elapsed_ms);
        drift->level_ms += alpha * (level_ms - drift->level_ms);
    }
    float error_ms = drift->level_ms - (float)c->target_frames * ms_per_frame;
    drift->integral = clamp(drift->integral + c->ki * error_ms * elapsed_ms * 1e-3f, c->max_ppm);
    drift->ppm = clamp(c->kp * error_ms + drift->integral, c->max_ppm);
    return drift->ppm;
}
