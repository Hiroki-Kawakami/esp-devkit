/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "audf_types.h"
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    AUDF_RESAMPLER_CUBIC = 0,   /*!< Catmull-Rom, no anti-aliasing: ratios near 1 */
    AUDF_RESAMPLER_POLYPHASE,   /*!< windowed-sinc, any ratio */
    AUDF_RESAMPLER_INTEGER,     /*!< windowed-sinc, one rate a multiple of the other */
} audf_resampler_kind_t;

#define AUDF_RESAMPLER_MAX_ADJUST_PPM 10000.0f

typedef struct audf_resampler audf_resampler_t;

typedef struct {
    audf_resampler_kind_t kind;
    audf_fmt_t fmt;
    uint8_t    channels;
    uint32_t   in_rate;
    uint32_t   out_rate;
    size_t     max_in_frames;   /*!< 0 = 1024 */
    uint16_t   taps;            /*!< sinc taps per output at the higher rate; 0 = 48 */
    uint16_t   phases;          /*!< POLYPHASE only, power of two up to 1024; 0 = 64 */
} audf_resampler_config_t;

esp_err_t audf_resampler_create(const audf_resampler_config_t *config, audf_resampler_t **out);
void      audf_resampler_destroy(audf_resampler_t *r);
void      audf_resampler_reset(audf_resampler_t *r);

/* Trims in_rate/out_rate; positive consumes input faster. Safe from any task,
 * clamped to AUDF_RESAMPLER_MAX_ADJUST_PPM. INTEGER returns NOT_SUPPORTED. */
esp_err_t audf_resampler_set_adjust(audf_resampler_t *r, float ppm);

/* Input frames that make the next process yield exactly out_frames. The ratio
 * is latched until that process. */
size_t audf_resampler_frames_needed(audf_resampler_t *r, size_t out_frames);

/* Buffer-sizing bounds, independent of state. */
size_t audf_resampler_max_out(const audf_resampler_t *r, size_t in_frames);
size_t audf_resampler_max_in(const audf_resampler_t *r, size_t out_frames);

/* Takes all of in (at most max_in_frames) and writes up to *out_frames;
 * *out_frames returns the count written. INVALID_SIZE when out was too small
 * to drain what in left behind. */
esp_err_t audf_resampler_process(audf_resampler_t *r, const void *in, size_t in_frames,
                                 void *out, size_t *out_frames);

#ifdef __cplusplus
}
#endif
