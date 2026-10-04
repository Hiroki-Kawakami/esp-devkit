/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "audf_codec.h"

#ifdef __cplusplus
extern "C" {
#endif

/* IMA ADPCM in the WAV block layout (format tag 0x11), S16 PCM. */
typedef struct {
    uint8_t  channels;
    uint16_t block_align;   /*!< bytes per block, a multiple of 4 * channels */
} audf_adpcm_config_t;

size_t audf_adpcm_block_frames(const audf_adpcm_config_t *config);

/* A short last block decodes to the frames it holds. */
esp_err_t audf_adpcm_decoder_create(const audf_adpcm_config_t *config, audf_decoder_t **out);
/* A short last input is padded with its final frame up to a whole group of
 * eight, so it decodes a few frames longer. */
esp_err_t audf_adpcm_encoder_create(const audf_adpcm_config_t *config, audf_encoder_t **out);

#ifdef __cplusplus
}
#endif
