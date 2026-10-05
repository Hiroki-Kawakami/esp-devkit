/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "audf_types.h"
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Normalised to a0 = 1. */
typedef struct {
    float b0, b1, b2;
    float a1, a2;
} audf_biquad_t;

audf_biquad_t audf_biquad_peaking(uint32_t fs, float f0, float q, float gain_db);
audf_biquad_t audf_biquad_low_shelf(uint32_t fs, float f0, float q, float gain_db);
audf_biquad_t audf_biquad_high_shelf(uint32_t fs, float f0, float q, float gain_db);
audf_biquad_t audf_biquad_lowpass(uint32_t fs, float f0, float q);
audf_biquad_t audf_biquad_highpass(uint32_t fs, float f0, float q);

typedef struct audf_eq audf_eq_t;

typedef struct {
    audf_fmt_t fmt;
    uint8_t    channels;
    size_t     max_stages;   /*!< 0 = 8 */
    bool       enabled;
    uint32_t   alloc_caps;   /*!< heap_caps_* for every allocation; 0 = malloc */
} audf_eq_config_t;

esp_err_t audf_eq_create(const audf_eq_config_t *config, audf_eq_t **out);
void      audf_eq_destroy(audf_eq_t *eq);

/* Must not run concurrently with process. Resets the filter state. */
esp_err_t audf_eq_reconfig(audf_eq_t *eq, audf_fmt_t fmt, uint8_t channels);

/* Setters are safe from any task and take effect at the next process. New
 * coefficients keep the filter state, so a slider can move without clicks. */
esp_err_t audf_eq_set_biquads(audf_eq_t *eq, const audf_biquad_t *biquads, size_t num_stages);
esp_err_t audf_eq_set_enabled(audf_eq_t *eq, bool enabled);
bool      audf_eq_get_enabled(audf_eq_t *eq);

/* in may equal out. */
void audf_eq_process(audf_eq_t *eq, const void *in, void *out, size_t frames);

#ifdef __cplusplus
}
#endif
