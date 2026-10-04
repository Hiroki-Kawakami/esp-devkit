/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "audf_codec.h"

typedef struct {
    esp_err_t (*decode)(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames);
    void (*destroy)(audf_decoder_t *dec);
} audf_decoder_ops_t;

struct audf_decoder {
    const audf_decoder_ops_t *ops;
    audf_fmt_t fmt;
    uint8_t    channels;
    size_t     max_frames;
};

typedef struct {
    esp_err_t (*encode)(audf_encoder_t *enc, const void *pcm, size_t frames, void *out, size_t *len);
    void (*destroy)(audf_encoder_t *enc);
} audf_encoder_ops_t;

struct audf_encoder {
    const audf_encoder_ops_t *ops;
    audf_fmt_t fmt;
    uint8_t    channels;
    size_t     frame_frames;
    size_t     max_bytes;
};
