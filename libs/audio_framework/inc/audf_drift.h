/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* PI controller from a buffer fill level to a resampler adjustment in ppm. */
typedef struct {
    uint32_t sample_rate;     /*!< rate the level is counted in */
    size_t   target_frames;
    float    kp;              /*!< ppm per ms of level error; 0 = 100 */
    float    ki;              /*!< ppm per ms of level error per second; 0 = 3 */
    float    max_ppm;         /*!< 0 = 1000 */
    uint32_t smoothing_ms;    /*!< level low-pass time constant; 0 = 500 */
} audf_drift_config_t;

typedef struct {
    audf_drift_config_t config;
    float level_ms;
    float integral;
    float ppm;
    bool  started;
} audf_drift_t;

void  audf_drift_init(audf_drift_t *drift, const audf_drift_config_t *config);
void  audf_drift_reset(audf_drift_t *drift);
/* elapsed_frames: frames the level's consumer took since the last update. */
float audf_drift_update(audf_drift_t *drift, size_t level, size_t elapsed_frames);

#ifdef __cplusplus
}
#endif
