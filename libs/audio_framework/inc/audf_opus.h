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

/* Opus with channel mapping family 0 (mono or stereo), decoded to S16 at
 * 48 kHz. One call takes one packet, up to 120 ms. */
#define AUDF_OPUS_RATE       48000
#define AUDF_OPUS_HEAD_BYTES 19

typedef struct {
    const uint8_t *head;       /*!< OpusHead, starting with its "OpusHead" magic */
    size_t         head_len;
    uint32_t       alloc_caps; /*!< heap_caps_* for every allocation; 0 = malloc */
} audf_opus_config_t;

/* The OpusHead output gain is applied; the pre-skip is left to the caller. */
esp_err_t audf_opus_decoder_create(const audf_opus_config_t *config, audf_decoder_t **out);
uint16_t  audf_opus_decoder_pre_skip(const audf_decoder_t *dec);
/* The range coder state after the last packet, as opus_demo records it. */
uint32_t  audf_opus_decoder_final_range(const audf_decoder_t *dec);

#ifdef __cplusplus
}
#endif
