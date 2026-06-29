/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal layout shared between the encoder dispatch shim (imgf_encoder.c)
 * and concrete encoders. Concrete encoders embed imgf_encoder as their first
 * member; the vtable upcasts the pointer to that base. Mirrors the
 * imgf_decoder_internal.h pattern.
 */

#pragma once

#include "imgf_encoder.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct imgf_encoder_vtable {
    imgf_err_t (*bind)    (imgf_encoder_t *e, uint8_t *dst, size_t cap);
    int        (*push_row)(imgf_encoder_t *e, const uint8_t *row);
    imgf_err_t (*finish)  (imgf_encoder_t *e, size_t *bytes_written);
    void       (*destroy) (imgf_encoder_t *e);
} imgf_encoder_vtable_t;

struct imgf_encoder {
    const imgf_encoder_vtable_t *vt;
    uint16_t      width;
    uint16_t      height;
    imgf_pixfmt_t input_pf;
    size_t        row_stride;       /* raw formats; 0 for stream/container */
    size_t        buffer_size_hint; /* raw exact size; container worst-case bound */
    imgf_err_t    last_error;
    uint8_t       bound;
};

#ifdef __cplusplus
}
#endif
