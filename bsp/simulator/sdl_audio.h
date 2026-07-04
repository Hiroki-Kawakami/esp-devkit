/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * SDL host audio backend, mirroring the board's caps so app audio code takes
 * the same paths as on device: PCM | SPEAKER over SDL queue-audio by default
 * (write() backpressures at ~100 ms queued, like a blocking I2S DMA write),
 * or a TONE-only square-wave buzzer with tone_only. Falls back to a silent
 * null sink with the same pacing when SIMULATOR_HEADLESS is set or no host
 * audio device opens.
 */

#pragma once
#include "bsp_audio.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    bool tone_only;  /*!< publish caps TONE instead of PCM | SPEAKER */
} sdl_audio_config_t;

/* Registers closed; the host audio device is opened on first use. NULL config
 * -> PCM | SPEAKER. */
esp_err_t sdl_audio_create(const sdl_audio_config_t *config, bsp_audio_t **out_audio);

#ifdef __cplusplus
}
#endif
