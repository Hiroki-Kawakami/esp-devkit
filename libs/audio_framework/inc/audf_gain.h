/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "audf_types.h"
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define AUDF_GAIN_MAX 127.0f

typedef struct audf_gain audf_gain_t;

typedef struct {
    audf_fmt_t fmt;
    uint8_t    channels;
    uint32_t   sample_rate;
    float      gain;          /*!< linear, initial */
} audf_gain_config_t;

esp_err_t audf_gain_create(const audf_gain_config_t *config, audf_gain_t **out);
void      audf_gain_destroy(audf_gain_t *gain);

/* Must not run concurrently with process. Keeps the gain and any fade. */
esp_err_t audf_gain_reconfig(audf_gain_t *gain, audf_fmt_t fmt, uint8_t channels, uint32_t sample_rate);

/* Linear, clamped to 0..AUDF_GAIN_MAX. Safe from any task; the fade runs
 * frame by frame inside process. fade_ms = 0 jumps, and a fade requested
 * before the next process starts from that jump. */
esp_err_t audf_gain_set(audf_gain_t *gain, float target, uint32_t fade_ms);
float     audf_gain_get(audf_gain_t *gain);

/* in may equal out. */
void audf_gain_process(audf_gain_t *gain, const void *in, void *out, size_t frames);

#ifdef __cplusplus
}
#endif
