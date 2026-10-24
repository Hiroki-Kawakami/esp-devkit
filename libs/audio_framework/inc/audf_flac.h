/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "audf_codec.h"

#ifdef __cplusplus
extern "C" {
#endif

/* FLAC up to 24 bits and 8 channels. 16 bits or fewer decode to S16, more to
 * S32, both scaled to full scale. */
#define AUDF_FLAC_STREAMINFO_BYTES 34

typedef struct {
    const uint8_t *streaminfo;     /*!< STREAMINFO block body, without its block header */
    size_t         streaminfo_len;
    uint32_t       alloc_caps;     /*!< heap_caps_* for every allocation; 0 = malloc */
} audf_flac_config_t;

/* A frame whose rate, channels or sample size differ from STREAMINFO fails, and
 * one that fails its CRC returns ESP_ERR_INVALID_CRC with pcm left untouched. */
esp_err_t audf_flac_decoder_create(const audf_flac_config_t *config, audf_decoder_t **out);
uint32_t  audf_flac_decoder_rate(const audf_decoder_t *dec);

#ifdef __cplusplus
}
#endif
