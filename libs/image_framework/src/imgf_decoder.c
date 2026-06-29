/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Dispatch shim for the imgf_decoder abstraction. Concrete decoders fill the
 * vtable; the public API forwards through it.
 */

#include <stddef.h>

#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"

imgf_err_t imgf_decoder_open(imgf_decoder_t *dec, imgf_stream_t src,
                             const imgf_decode_opts_t *opts) {
    if (!dec) return IMGF_ERR_INVALID_ARG;
    if (dec->opened) return IMGF_ERR_INVALID_STATE;
    imgf_decode_opts_t zero = {0};
    imgf_err_t err = dec->vt->open(dec, src, opts ? opts : &zero);
    dec->last_error = err;
    if (err == IMGF_OK) dec->opened = 1;
    return err;
}

bool imgf_decoder_next_row(imgf_decoder_t *dec, uint8_t *dst) {
    if (!dec || !dec->opened || !dst) return false;
    return dec->vt->next_row(dec, dst);
}

uint16_t imgf_decoder_width(const imgf_decoder_t *dec) {
    return dec ? dec->width : 0;
}

uint16_t imgf_decoder_height(const imgf_decoder_t *dec) {
    return dec ? dec->height : 0;
}

imgf_pixfmt_t imgf_decoder_pixfmt(const imgf_decoder_t *dec) {
    return dec ? dec->pixfmt : IMGF_PIX_INHERIT;
}

imgf_err_t imgf_decoder_last_error(const imgf_decoder_t *dec) {
    return dec ? dec->last_error : IMGF_ERR_INVALID_ARG;
}

void imgf_decoder_destroy(imgf_decoder_t *dec) {
    if (dec) dec->vt->destroy(dec);
}
