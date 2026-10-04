/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_codec_internal.h"

esp_err_t audf_decoder_decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    if (!dec || !frame || !pcm || !frames) return ESP_ERR_INVALID_ARG;
    return dec->ops->decode(dec, frame, len, pcm, frames);
}

size_t audf_decoder_max_frames(const audf_decoder_t *dec) { return dec->max_frames; }
audf_fmt_t audf_decoder_fmt(const audf_decoder_t *dec) { return dec->fmt; }
uint8_t audf_decoder_channels(const audf_decoder_t *dec) { return dec->channels; }

void audf_decoder_destroy(audf_decoder_t *dec) {
    if (dec) dec->ops->destroy(dec);
}

esp_err_t audf_encoder_encode(audf_encoder_t *enc, const void *pcm, size_t frames, void *out, size_t *len) {
    if (!enc || !pcm || !out || !len) return ESP_ERR_INVALID_ARG;
    if (!frames || frames > enc->frame_frames) return ESP_ERR_INVALID_SIZE;
    return enc->ops->encode(enc, pcm, frames, out, len);
}

size_t audf_encoder_frame_frames(const audf_encoder_t *enc) { return enc->frame_frames; }
size_t audf_encoder_max_bytes(const audf_encoder_t *enc) { return enc->max_bytes; }
audf_fmt_t audf_encoder_fmt(const audf_encoder_t *enc) { return enc->fmt; }
uint8_t audf_encoder_channels(const audf_encoder_t *enc) { return enc->channels; }

void audf_encoder_destroy(audf_encoder_t *enc) {
    if (enc) enc->ops->destroy(enc);
}
