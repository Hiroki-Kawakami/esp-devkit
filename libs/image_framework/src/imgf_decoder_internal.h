/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal layout shared between the dispatch shim (imgf_decoder.c) and the
 * concrete decoders. Concrete structs embed imgf_decoder as their first member
 * and the vtable upcasts the pointer to that base.
 */

#pragma once

#include "imgf_decoder.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct imgf_decoder_vtable {
    imgf_err_t (*open)    (imgf_decoder_t *dec, imgf_stream_t src,
                           const imgf_decode_opts_t *opts);
    bool       (*next_row)(imgf_decoder_t *dec, uint8_t *dst);
    void       (*destroy) (imgf_decoder_t *dec);
} imgf_decoder_vtable_t;

struct imgf_decoder {
    const imgf_decoder_vtable_t *vt;
    uint16_t      width;
    uint16_t      height;
    imgf_pixfmt_t pixfmt;
    imgf_err_t    last_error;
    uint8_t       opened;
};

#ifdef __cplusplus
}
#endif
