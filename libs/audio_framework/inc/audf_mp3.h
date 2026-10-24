/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "audf_codec.h"

#ifdef __cplusplus
extern "C" {
#endif

/* MPEG-1, 2 and 2.5 audio Layer II and III, decoded to S16. One call takes one
 * whole frame, header included; Layer III carries its bit reservoir from one
 * call to the next. */
#define AUDF_MP3_HEADER_BYTES 4
#define AUDF_MP3_MAX_FRAMES   1152

typedef struct {
    uint32_t rate;
    uint16_t bytes;    /*!< whole frame, header included */
    uint16_t frames;   /*!< PCM frames it decodes to */
    uint8_t  channels;
    uint8_t  layer;
} audf_mp3_header_t;

typedef struct {
    uint8_t  channels;   /*!< output channels, 1 or 2 */
    uint32_t alloc_caps; /*!< heap_caps_* for every allocation; 0 = malloc */
} audf_mp3_config_t;

/* False for Layer I, free format and reserved fields. */
bool      audf_mp3_parse_header(const uint8_t *p, audf_mp3_header_t *out);
/* A frame with the other channel count is mixed down or doubled. A Layer III
 * frame whose reservoir was lost (the first after a reset) decodes as silence. */
esp_err_t audf_mp3_decoder_create(const audf_mp3_config_t *config, audf_decoder_t **out);
/* The sample rate of the last decoded frame. */
uint32_t  audf_mp3_decoder_rate(const audf_decoder_t *dec);
/* Runs the SIMD kernels against their C versions on random input; returns the
 * number of mismatching calls (-1 without memory), always 0 where there is no
 * SIMD version. */
int       audf_mp3_kernel_selftest(uint32_t seed, int iterations);

#ifdef __cplusplus
}
#endif
