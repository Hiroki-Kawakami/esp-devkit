/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Output sink dispatch. Raw formats (L8 / I4 / I1 / RGB888 / RGB565) pack each
 * row directly into the destination buffer at a fixed stride; JPEG is delegated
 * to imgf_jpege which buffers MCU rows and emits compressed entropy data.
 */

#include "imgf_encoder.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_jpege.h"

struct imgf_encoder {
    imgf_enc_format_t fmt;
    imgf_pixfmt_t     input_pf;
    uint16_t w, h;

    /* raw formats */
    size_t   stride;
    uint8_t *dst;
    size_t   dst_cap;
    int      row;
    uint8_t  bound;

    /* JPEG (NULL for raw formats) */
    imgf_jpege_t *jpege;

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
        case IMGF_ENC_JPEG:   return 0;   /* variable-rate */
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
        case IMGF_ENC_JPEG:   return IMGF_PIX_INHERIT;   /* Gray8 or RGB888 */
        default:              return IMGF_PIX_INHERIT;
    }
}

/* ---- create / destroy ------------------------------------------------- */

static imgf_err_t resolve_input_pf(imgf_enc_format_t fmt, imgf_pixfmt_t requested,
                                   imgf_pixfmt_t *resolved) {
    imgf_pixfmt_t expected = imgf_encoder_format_input_pixfmt(fmt);
    if (fmt == IMGF_ENC_JPEG) {
        if (requested == IMGF_PIX_INHERIT) requested = IMGF_PIX_RGB888;
        if (requested != IMGF_PIX_GRAY8 && requested != IMGF_PIX_RGB888)
            return IMGF_ERR_UNSUPPORTED;
        *resolved = requested;
        return IMGF_OK;
    }
    /* raw format: must match the fixed expected pixfmt (or INHERIT = take it) */
    if (requested == IMGF_PIX_INHERIT) { *resolved = expected; return IMGF_OK; }
    if (requested != expected) return IMGF_ERR_INVALID_ARG;
    *resolved = requested;
    return IMGF_OK;
}

imgf_encoder_t *imgf_encoder_create(imgf_enc_format_t fmt,
                                    uint16_t width, uint16_t height,
                                    imgf_pixfmt_t input_pixfmt,
                                    const imgf_encoder_opts_t *opts,
                                    imgf_err_t *out_err) {
    if (width == 0 || height == 0) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    imgf_pixfmt_t pf;
    imgf_err_t e = resolve_input_pf(fmt, input_pixfmt, &pf);
    if (e != IMGF_OK) { if (out_err) *out_err = e; return NULL; }

    imgf_encoder_t *enc = (imgf_encoder_t *)calloc(1, sizeof *enc);
    if (!enc) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
    enc->fmt      = fmt;
    enc->input_pf = pf;
    enc->w        = width;
    enc->h        = height;
    enc->stride   = imgf_encoder_format_stride(fmt, width);

    if (fmt == IMGF_ENC_JPEG) {
        int q = opts ? opts->jpeg_quality : 0;
        int sub = opts ? (int)opts->jpeg_subsample : IMGF_JPEG_SUBSAMPLE_DEFAULT;
        uint32_t caps = opts ? opts->alloc_caps : 0;
        enc->jpege = imgf_jpege_create(width, height, pf, q, sub, caps, &e);
        if (!enc->jpege) {
            free(enc);
            if (out_err) *out_err = e;
            return NULL;
        }
    }

    if (out_err) *out_err = IMGF_OK;
    return enc;
}

void imgf_encoder_destroy(imgf_encoder_t *e) {
    if (!e) return;
    if (e->jpege) imgf_jpege_destroy(e->jpege);
    free(e);
}

size_t        imgf_encoder_row_stride  (const imgf_encoder_t *e) { return e ? e->stride : 0; }
imgf_pixfmt_t imgf_encoder_input_pixfmt(const imgf_encoder_t *e) { return e ? e->input_pf : IMGF_PIX_INHERIT; }
imgf_err_t    imgf_encoder_last_error  (const imgf_encoder_t *e) { return e ? e->last_error : IMGF_ERR_INVALID_ARG; }

size_t imgf_encoder_buffer_size(const imgf_encoder_t *e) {
    if (!e) return 0;
    if (e->fmt == IMGF_ENC_JPEG) return imgf_jpege_buffer_upper_bound(e->w, e->h, e->input_pf);
    return e->stride * e->h;
}

imgf_err_t imgf_encoder_bind_buffer(imgf_encoder_t *e, uint8_t *dst, size_t cap) {
    if (!e || !dst) return IMGF_ERR_INVALID_ARG;
    if (e->fmt == IMGF_ENC_JPEG) {
        imgf_err_t er = imgf_jpege_bind(e->jpege, dst, cap);
        if (er != IMGF_OK) { e->last_error = er; return er; }
        e->dst = dst;
        e->dst_cap = cap;
        e->row = 0;
        e->bound = 1;
        return IMGF_OK;
    }
    if (cap < e->stride * e->h) return IMGF_ERR_INVALID_ARG;
    e->dst = dst;
    e->dst_cap = cap;
    e->row = 0;
    e->bound = 1;
    return IMGF_OK;
}

/* ---- raw-format packers ----------------------------------------------- */

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

    if (e->fmt == IMGF_ENC_JPEG) {
        imgf_err_t er = imgf_jpege_push(e->jpege, row);
        if (er != IMGF_OK) { e->last_error = er; return -1; }
        e->row++;
        return 1;
    }

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
    if (e->fmt == IMGF_ENC_JPEG) {
        size_t n = 0;
        imgf_err_t er = imgf_jpege_finish(e->jpege, &n);
        if (er != IMGF_OK) { e->last_error = er; return er; }
        if (bytes_written) *bytes_written = n;
        return IMGF_OK;
    }
    if (e->row != e->h) return IMGF_ERR_INVALID_STATE;
    if (bytes_written) *bytes_written = (size_t)e->row * e->stride;
    return IMGF_OK;
}

/* ---- Layer 2 ---------------------------------------------------------- */

imgf_err_t imgf_encode_buffer(imgf_enc_format_t fmt,
                              const uint8_t *src, uint16_t w, uint16_t h,
                              size_t src_stride, imgf_pixfmt_t src_pf,
                              uint8_t *dst, size_t dst_capacity,
                              size_t *out_bytes,
                              const imgf_encoder_opts_t *opts) {
    if (!src || !dst || w == 0 || h == 0) return IMGF_ERR_INVALID_ARG;

    imgf_err_t err;
    imgf_encoder_t *e = imgf_encoder_create(fmt, w, h, src_pf, opts, &err);
    if (!e) return err;

    imgf_pixfmt_t pf = imgf_encoder_input_pixfmt(e);
    int src_bpp = imgf_pixfmt_bpp(pf);
    size_t src_row_bytes = (size_t)w * src_bpp;
    if (fmt == IMGF_ENC_I4 || fmt == IMGF_ENC_I1) src_row_bytes = (size_t)w;
    if (src_stride == 0) src_stride = src_row_bytes;
    if (src_stride < src_row_bytes) { imgf_encoder_destroy(e); return IMGF_ERR_INVALID_ARG; }

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
