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

typedef struct audf_mixer audf_mixer_t;

typedef struct {
    audf_fmt_t     fmt;
    uint8_t        out_channels;
    uint8_t        num_inputs;
    const uint8_t *in_channels;   /*!< [num_inputs] */
} audf_mixer_config_t;

/* Every input starts with a pass-through matrix: identity, 1->N copies,
 * N->1 averages. */
esp_err_t audf_mixer_create(const audf_mixer_config_t *config, audf_mixer_t **out);
void      audf_mixer_destroy(audf_mixer_t *mixer);

/* matrix is out_channels x in_channels, row-major. Safe from any task; takes
 * effect at the next process. */
esp_err_t audf_mixer_set_matrix(audf_mixer_t *mixer, uint8_t input, const float *matrix);
esp_err_t audf_mixer_get_matrix(audf_mixer_t *mixer, uint8_t input, float *matrix);

uint8_t audf_mixer_num_inputs(const audf_mixer_t *mixer);

/* A NULL input mixes as silence. out may alias an input that has at least
 * out_channels channels. */
void audf_mixer_process(audf_mixer_t *mixer, const void *const in[], void *out, size_t frames);

#ifdef __cplusplus
}
#endif
