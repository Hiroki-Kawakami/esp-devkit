/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Raw-format encoders (L8 / I4 / I1 / RGB888 / RGB565). Each push_row packs the
 * caller's row into the bound destination buffer at the next row offset; no
 * compression, no container — just byte packing. PNG / JPEG implementations
 * plug into the same vtable when added.
 */

#include "imgf_encoder.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

struct imgf_encoder {
    imgf_enc_format_t fmt;
    imgf_pixfmt_t     input_pf;
    uint16_t w, h;
    size_t   stride;
    uint8_t *dst;
    size_t   dst_cap;
    int      row;
    uint8_t  bound;
    imgf_err_t last_error;
};

/* ---- format metadata -------------------------------------------------- */

size_t imgf_encoder_format_stride(imgf_enc_format_t fmt, uint16_t width) {
    switch (fmt) {
        case IMGF_ENC_L8:     return (size_t)width;
        case IMGF_ENC_I4:     return ((size_t)width + 1) / 2;
        case IMGF_ENC_I1:     return ((size_t)width + 7) / 8;
        case IMGF_ENC_RGB888: return (size_t)width * 3;
        case IMGF_ENC_RGB565: return (size_t)width * 2;
        default:              return 0;
    }
}

imgf_pixfmt_t imgf_encoder_format_input_pixfmt(imgf_enc_format_t fmt) {
    switch (fmt) {
        case IMGF_ENC_L8:
        case IMGF_ENC_I4:
        case IMGF_ENC_I1:     return IMGF_PIX_GRAY8;
        case IMGF_ENC_RGB888: return IMGF_PIX_RGB888;
        case IMGF_ENC_RGB565: return IMGF_PIX_RGB565;
        default:              return IMGF_PIX_INHERIT;
    }
}

/* ---- create / destroy ------------------------------------------------- */

imgf_encoder_t *imgf_encoder_create(imgf_enc_format_t fmt,
                                    uint16_t width, uint16_t height,
                                    const imgf_encoder_opts_t *opts,
                                    imgf_err_t *out_err) {
    (void)opts;
    if (width == 0 || height == 0) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    imgf_pixfmt_t pf = imgf_encoder_format_input_pixfmt(fmt);
    if (pf == IMGF_PIX_INHERIT) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    imgf_encoder_t *e = (imgf_encoder_t *)calloc(1, sizeof *e);
    if (!e) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
    e->fmt      = fmt;
    e->input_pf = pf;
    e->w        = width;
    e->h        = height;
    e->stride   = imgf_encoder_format_stride(fmt, width);
    if (out_err) *out_err = IMGF_OK;
    return e;
}

void imgf_encoder_destroy(imgf_encoder_t *e) { free(e); }

size_t        imgf_encoder_row_stride  (const imgf_encoder_t *e) { return e ? e->stride : 0; }
size_t        imgf_encoder_buffer_size (const imgf_encoder_t *e) { return e ? e->stride * e->h : 0; }
imgf_pixfmt_t imgf_encoder_input_pixfmt(const imgf_encoder_t *e) { return e ? e->input_pf : IMGF_PIX_INHERIT; }
imgf_err_t    imgf_encoder_last_error  (const imgf_encoder_t *e) { return e ? e->last_error : IMGF_ERR_INVALID_ARG; }

imgf_err_t imgf_encoder_bind_buffer(imgf_encoder_t *e, uint8_t *dst, size_t cap) {
    if (!e || !dst) return IMGF_ERR_INVALID_ARG;
    if (cap < e->stride * e->h) return IMGF_ERR_INVALID_ARG;
    e->dst = dst;
    e->dst_cap = cap;
    e->row = 0;
    e->bound = 1;
    return IMGF_OK;
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

/* ---- push / finish ---------------------------------------------------- */

int imgf_encoder_push_row(imgf_encoder_t *e, const uint8_t *row) {
    if (!e || !row) return -1;
    if (!e->bound) { e->last_error = IMGF_ERR_INVALID_STATE; return -1; }
    if (e->row >= e->h) { e->last_error = IMGF_ERR_INVALID_STATE; return -1; }

    uint8_t *dst = e->dst + (size_t)e->row * e->stride;
    switch (e->fmt) {
        case IMGF_ENC_L8:     pack_l8    (dst, row, e->w); break;
        case IMGF_ENC_I4:     pack_i4    (dst, row, e->w); break;
        case IMGF_ENC_I1:     pack_i1    (dst, row, e->w); break;
        case IMGF_ENC_RGB888: pack_rgb888(dst, row, e->w); break;
        case IMGF_ENC_RGB565: pack_rgb565(dst, row, e->w); break;
        default:
            e->last_error = IMGF_ERR_UNSUPPORTED;
            return -1;
    }
    e->row++;
    return 1;
}

imgf_err_t imgf_encoder_finish(imgf_encoder_t *e, size_t *bytes_written) {
    if (!e) return IMGF_ERR_INVALID_ARG;
    if (!e->bound) return IMGF_ERR_INVALID_STATE;
    if (e->row != e->h) return IMGF_ERR_INVALID_STATE;
    /* Raw formats have no trailer. PNG/JPEG implementations will write here. */
    if (bytes_written) *bytes_written = (size_t)e->row * e->stride;
    return IMGF_OK;
}

/* ---- Layer 2 ---------------------------------------------------------- */

imgf_err_t imgf_encode_buffer(imgf_enc_format_t fmt,
                              const uint8_t *src, uint16_t w, uint16_t h,
                              size_t src_stride,
                              uint8_t *dst, size_t dst_capacity,
                              size_t *out_bytes,
                              const imgf_encoder_opts_t *opts) {
    if (!src || !dst || w == 0 || h == 0) return IMGF_ERR_INVALID_ARG;
    imgf_pixfmt_t input_pf = imgf_encoder_format_input_pixfmt(fmt);
    if (input_pf == IMGF_PIX_INHERIT) return IMGF_ERR_INVALID_ARG;
    size_t src_row_bytes = (size_t)w * imgf_pixfmt_bpp(input_pf);
    if (src_stride == 0) src_stride = src_row_bytes;
    if (src_stride < src_row_bytes) return IMGF_ERR_INVALID_ARG;
    /* I4/I1 expect Gray8-shaped indices; row bytes equal w. */
    if (fmt == IMGF_ENC_I4 || fmt == IMGF_ENC_I1) {
        if (src_stride < (size_t)w) return IMGF_ERR_INVALID_ARG;
    }

    imgf_err_t err;
    imgf_encoder_t *e = imgf_encoder_create(fmt, w, h, opts, &err);
    if (!e) return err;
    err = imgf_encoder_bind_buffer(e, dst, dst_capacity);
    if (err != IMGF_OK) { imgf_encoder_destroy(e); return err; }

    for (uint16_t y = 0; y < h; y++) {
        if (imgf_encoder_push_row(e, src + (size_t)y * src_stride) < 0) {
            err = imgf_encoder_last_error(e);
            imgf_encoder_destroy(e);
            return err;
        }
    }
    err = imgf_encoder_finish(e, out_bytes);
    imgf_encoder_destroy(e);
    return err;
}
