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

/* ALAC up to 24 bits and 8 channels. 16 bits or fewer decode to S16, more to
 * S32, both scaled to full scale. Channels come in the order of the frame's
 * elements. */
#define AUDF_ALAC_COOKIE_BYTES 24

typedef struct {
    const uint8_t *cookie;      /*!< ALACSpecificConfig, without any atom header */
    size_t         cookie_len;
    uint32_t       alloc_caps;  /*!< heap_caps_* for every allocation; 0 = malloc */
} audf_alac_config_t;

esp_err_t audf_alac_decoder_create(const audf_alac_config_t *config, audf_decoder_t **out);
uint32_t  audf_alac_decoder_rate(const audf_decoder_t *dec);

#ifdef __cplusplus
}
#endif
