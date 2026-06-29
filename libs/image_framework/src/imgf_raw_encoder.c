/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Raw encoders: per-row packing into the bound destination buffer at a fixed
 * stride. The five formats share the same struct + vtable; only the inner
 * pack call differs.
 */

#include "imgf_raw_encoder.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_encoder.h"
#include "imgf_encoder_internal.h"

typedef struct {
    imgf_encoder_t   base;
    imgf_raw_format_t fmt;
    uint8_t *dst;
    size_t   dst_cap;
    int      row;
} raw_enc_t;

size_t imgf_raw_format_stride(imgf_raw_format_t fmt, uint16_t width) {
    switch (fmt) {
        case IMGF_RAW_L8:     return (size_t)width;
        case IMGF_RAW_I4:     return ((size_t)width + 1) / 2;
        case IMGF_RAW_I1:     return ((size_t)width + 7) / 8;
        case IMGF_RAW_RGB888: return (size_t)width * 3;
        case IMGF_RAW_RGB565: return (size_t)width * 2;
        default:              return 0;
    }
}

imgf_pixfmt_t imgf_raw_format_input_pixfmt(imgf_raw_format_t fmt) {
    switch (fmt) {
        case IMGF_RAW_L8:
        case IMGF_RAW_I4:
        case IMGF_RAW_I1:     return IMGF_PIX_GRAY8;
        case IMGF_RAW_RGB888: return IMGF_PIX_RGB888;
        case IMGF_RAW_RGB565: return IMGF_PIX_RGB565;
        default:              return IMGF_PIX_INHERIT;
    }
}

/* ---- per-format packers ----------------------------------------------- */

static void pack_l8(uint8_t *dst, const uint8_t *src, int w) {
    memcpy(dst, src, (size_t)w);
}

static void pack_i4(uint8_t *dst, const uint8_t *src, int w) {
    int sb = w / 2;
    for (int i = 0; i < sb; i++) {
        uint8_t hi = src[2 * i] & 0x0F;
        uint8_t lo = src[2 * i + 1] & 0x0F;
        dst[i] = (uint8_t)((hi << 4) | lo);
    }
    if (w & 1) dst[sb] = (uint8_t)((src[w - 1] & 0x0F) << 4);
}

static void pack_i1(uint8_t *dst, const uint8_t *src, int w) {
    int sb = w / 8;
    for (int i = 0; i < sb; i++) {
        uint8_t b = 0;
        for (int k = 0; k < 8; k++) if (src[i * 8 + k] & 1) b |= (uint8_t)(0x80 >> k);
        dst[i] = b;
    }
    if (w & 7) {
        uint8_t b = 0;
        for (int k = 0; k < (w & 7); k++) if (src[sb * 8 + k] & 1) b |= (uint8_t)(0x80 >> k);
        dst[sb] = b;
    }
}

static void pack_rgb888(uint8_t *dst, const uint8_t *src, int w) {
    memcpy(dst, src, (size_t)w * 3);
}

static void pack_rgb565(uint8_t *dst, const uint8_t *src, int w) {
    memcpy(dst, src, (size_t)w * 2);
}

/* ---- vtable ----------------------------------------------------------- */

static imgf_err_t raw_bind(imgf_encoder_t *base, uint8_t *dst, size_t cap) {
    raw_enc_t *r = (raw_enc_t *)base;
    if (cap < base->row_stride * base->height) return IMGF_ERR_INVALID_ARG;
    r->dst = dst;
    r->dst_cap = cap;
    r->row = 0;
    return IMGF_OK;
}

static int raw_push(imgf_encoder_t *base, const uint8_t *row) {
    raw_enc_t *r = (raw_enc_t *)base;
    if (r->row >= base->height) { base->last_error = IMGF_ERR_INVALID_STATE; return -1; }
    uint8_t *dst = r->dst + (size_t)r->row * base->row_stride;
    switch (r->fmt) {
        case IMGF_RAW_L8:     pack_l8    (dst, row, base->width); break;
        case IMGF_RAW_I4:     pack_i4    (dst, row, base->width); break;
        case IMGF_RAW_I1:     pack_i1    (dst, row, base->width); break;
        case IMGF_RAW_RGB888: pack_rgb888(dst, row, base->width); break;
        case IMGF_RAW_RGB565: pack_rgb565(dst, row, base->width); break;
        default:
            base->last_error = IMGF_ERR_UNSUPPORTED;
            return -1;
    }
    r->row++;
    return 1;
}

static imgf_err_t raw_finish(imgf_encoder_t *base, size_t *bytes_written) {
    raw_enc_t *r = (raw_enc_t *)base;
    if (r->row != base->height) return IMGF_ERR_INVALID_STATE;
    if (bytes_written) *bytes_written = (size_t)r->row * base->row_stride;
    return IMGF_OK;
}

static void raw_destroy(imgf_encoder_t *base) {
    free(base);
}

static const imgf_encoder_vtable_t k_raw_vt = {
    raw_bind, raw_push, raw_finish, raw_destroy,
};

/* ---- factory ---------------------------------------------------------- */

imgf_encoder_t *imgf_raw_encoder_create(imgf_raw_format_t fmt,
                                        uint16_t width, uint16_t height,
                                        const imgf_raw_encoder_opts_t *opts,
                                        imgf_err_t *out_err) {
    (void)opts;
    if (width == 0 || height == 0) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    imgf_pixfmt_t pf = imgf_raw_format_input_pixfmt(fmt);
    if (pf == IMGF_PIX_INHERIT) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    raw_enc_t *r = (raw_enc_t *)calloc(1, sizeof *r);
    if (!r) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
    r->base.vt = &k_raw_vt;
    r->base.width  = width;
    r->base.height = height;
    r->base.input_pf = pf;
    r->base.row_stride = imgf_raw_format_stride(fmt, width);
    r->base.buffer_size_hint = r->base.row_stride * height;
    r->fmt = fmt;
    if (out_err) *out_err = IMGF_OK;
    return &r->base;
}
