/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Dispatch shim for the imgf_encoder abstraction. Concrete factories
 * (imgf_jpege_create, imgf_raw_encoder_create) build their format-specific
 * struct with imgf_encoder embedded as the first member; this file only
 * forwards through the vtable.
 */

#include <stddef.h>

#include "imgf_encoder.h"
#include "imgf_encoder_internal.h"

imgf_err_t imgf_encoder_bind_buffer(imgf_encoder_t *e, uint8_t *dst, size_t cap) {
    if (!e || !dst) return IMGF_ERR_INVALID_ARG;
    imgf_err_t err = e->vt->bind(e, dst, cap);
    e->last_error = err;
    if (err == IMGF_OK) e->bound = 1;
    return err;
}

int imgf_encoder_push_row(imgf_encoder_t *e, const uint8_t *row) {
    if (!e || !row) return -1;
    if (!e->bound) { e->last_error = IMGF_ERR_INVALID_STATE; return -1; }
    return e->vt->push_row(e, row);
}

imgf_err_t imgf_encoder_finish(imgf_encoder_t *e, size_t *bytes_written) {
    if (!e) return IMGF_ERR_INVALID_ARG;
    if (!e->bound) return IMGF_ERR_INVALID_STATE;
    return e->vt->finish(e, bytes_written);
}

size_t        imgf_encoder_row_stride  (const imgf_encoder_t *e) { return e ? e->row_stride : 0; }
size_t        imgf_encoder_buffer_size (const imgf_encoder_t *e) { return e ? e->buffer_size_hint : 0; }
imgf_pixfmt_t imgf_encoder_input_pixfmt(const imgf_encoder_t *e) { return e ? e->input_pf : IMGF_PIX_INHERIT; }
imgf_err_t    imgf_encoder_last_error  (const imgf_encoder_t *e) { return e ? e->last_error : IMGF_ERR_INVALID_ARG; }

void imgf_encoder_destroy(imgf_encoder_t *e) {
    if (e) e->vt->destroy(e);
}

imgf_err_t imgf_encoder_encode_buffer(imgf_encoder_t *e,
                                      const uint8_t *src, size_t src_stride,
                                      uint8_t *dst, size_t dst_cap,
                                      size_t *out_bytes) {
    if (!e || !src || !dst) return IMGF_ERR_INVALID_ARG;
    int bpp = imgf_pixfmt_bpp(e->input_pf);
    if (bpp == 0) bpp = 1;   /* gray-shaped indices (I4/I1) */
    size_t row_bytes = (size_t)e->width * bpp;
    if (src_stride == 0) src_stride = row_bytes;
    if (src_stride < row_bytes) return IMGF_ERR_INVALID_ARG;

    imgf_err_t err = imgf_encoder_bind_buffer(e, dst, dst_cap);
    if (err != IMGF_OK) return err;
    for (uint16_t y = 0; y < e->height; y++) {
        if (imgf_encoder_push_row(e, src + (size_t)y * src_stride) < 0)
            return imgf_encoder_last_error(e);
    }
    return imgf_encoder_finish(e, out_bytes);
}
