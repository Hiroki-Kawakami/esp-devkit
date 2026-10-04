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

/* Codecs work on one encoded frame at a time; containers are parsed by the
 * caller. */
typedef struct audf_decoder audf_decoder_t;
typedef struct audf_encoder audf_encoder_t;

/* pcm holds audf_decoder_max_frames() frames. */
esp_err_t  audf_decoder_decode(audf_decoder_t *dec, const void *frame, size_t len,
                               void *pcm, size_t *frames);
size_t     audf_decoder_max_frames(const audf_decoder_t *dec);
audf_fmt_t audf_decoder_fmt(const audf_decoder_t *dec);
uint8_t    audf_decoder_channels(const audf_decoder_t *dec);
void       audf_decoder_destroy(audf_decoder_t *dec);

/* frames is audf_encoder_frame_frames() except for the last call; out holds
 * audf_encoder_max_bytes() bytes. */
esp_err_t  audf_encoder_encode(audf_encoder_t *enc, const void *pcm, size_t frames,
                               void *out, size_t *len);
size_t     audf_encoder_frame_frames(const audf_encoder_t *enc);
size_t     audf_encoder_max_bytes(const audf_encoder_t *enc);
audf_fmt_t audf_encoder_fmt(const audf_encoder_t *enc);
uint8_t    audf_encoder_channels(const audf_encoder_t *enc);
void       audf_encoder_destroy(audf_encoder_t *enc);

#ifdef __cplusplus
}
#endif
