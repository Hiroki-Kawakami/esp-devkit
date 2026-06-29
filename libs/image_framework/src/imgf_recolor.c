/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_recolor.h"

#include <math.h>
#include <stdlib.h>

#include "imgf_alloc.h"

struct imgf_recolor {
    uint16_t in_lut[256];   /* channel value -> linear (0..65535) */
    uint8_t  out_lut[256];  /* linear>>8 -> perceptual gray (0..255) */
    uint32_t wr, wg, wb;    /* luma weights, Q15 (sum == 32768) */
    bool     linear_downsample;
    bool     invert;
};

static float srgb_to_linear(float c) {
    return c <= 0.04045f ? c / 12.92f : powf((c + 0.055f) / 1.055f, 2.4f);
}

static float linear_to_srgb(float c) {
    return c <= 0.0031308f ? 12.92f * c : 1.055f * powf(c, 1.0f / 2.4f) - 0.055f;
}

static float clamp01(float v) { return v < 0.0f ? 0.0f : (v > 1.0f ? 1.0f : v); }

imgf_recolor_t *imgf_recolor_create(const imgf_recolor_opts_t *opts, imgf_err_t *out_err) {
    static const imgf_recolor_opts_t kDefault = {
        .luma = IMGF_LUMA_REC709, .in_gamma = IMGF_GAMMA_IN_SRGB,
        .out_gamma = IMGF_GAMMA_OUT_SRGB, .in_power = 2.2f, .out_power = 2.2f,
        .linear_downsample = true,
    };
    if (!opts) opts = &kDefault;

    imgf_recolor_t *rc = (imgf_recolor_t *)imgf_alloc(sizeof *rc, 0);
    if (!rc) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }

    rc->linear_downsample = opts->linear_downsample;
    rc->invert = opts->invert;

    /* Input EOTF -> linear (0..65535). */
    for (int i = 0; i < 256; i++) {
        float c = i / 255.0f, lin;
        switch (opts->in_gamma) {
            case IMGF_GAMMA_IN_POWER:  lin = powf(c, opts->in_power); break;
            case IMGF_GAMMA_IN_LINEAR: lin = c; break;
            case IMGF_GAMMA_IN_SRGB:   default: lin = srgb_to_linear(c); break;
        }
        rc->in_lut[i] = (uint16_t)lroundf(clamp01(lin) * 65535.0f);
    }

    /* linear (indexed by top 8 bits) -> perceptual gray (0..255). */
    for (int i = 0; i < 256; i++) {
        float lin = i / 255.0f, g;
        switch (opts->out_gamma) {
            case IMGF_GAMMA_OUT_POWER: g = powf(lin, 1.0f / opts->out_power); break;
            case IMGF_GAMMA_OUT_EPDLUT:
                if (opts->epd_lut16) {
                    float fk = lin * 15.0f;
                    int k = (int)fk;
                    if (k >= 15) g = opts->epd_lut16[15] / 255.0f;
                    else {
                        float t = fk - k;
                        g = ((1.0f - t) * opts->epd_lut16[k] + t * opts->epd_lut16[k + 1]) / 255.0f;
                    }
                } else {
                    g = linear_to_srgb(lin);
                }
                break;
            case IMGF_GAMMA_OUT_SRGB: default: g = linear_to_srgb(lin); break;
        }
        rc->out_lut[i] = (uint8_t)lroundf(clamp01(g) * 255.0f);
    }

    /* Luma weights -> Q15, forcing the sum to 32768 (wb absorbs rounding). */
    float w[3];
    switch (opts->luma) {
        case IMGF_LUMA_REC601:  w[0] = 0.299f;  w[1] = 0.587f;  w[2] = 0.114f;  break;
        case IMGF_LUMA_AVERAGE: w[0] = w[1] = w[2] = 1.0f / 3.0f; break;
        case IMGF_LUMA_CUSTOM:
            w[0] = opts->luma_custom[0]; w[1] = opts->luma_custom[1]; w[2] = opts->luma_custom[2];
            break;
        case IMGF_LUMA_REC709: default: w[0] = 0.2126f; w[1] = 0.7152f; w[2] = 0.0722f; break;
    }
    float sum = w[0] + w[1] + w[2];
    if (sum <= 0.0f) { w[0] = 0.2126f; w[1] = 0.7152f; w[2] = 0.0722f; sum = 1.0f; }
    rc->wr = (uint32_t)lroundf(w[0] / sum * 32768.0f);
    rc->wg = (uint32_t)lroundf(w[1] / sum * 32768.0f);
    rc->wb = 32768u - rc->wr - rc->wg;

    if (out_err) *out_err = IMGF_OK;
    return rc;
}

void imgf_recolor_destroy(imgf_recolor_t *rc) { imgf_free(rc); }

void imgf_recolor_to_intensity(const imgf_recolor_t *rc, const uint8_t *src,
                               int width, imgf_pixfmt_t src_pf, uint8_t *intensity) {
    bool gray = (src_pf == IMGF_PIX_GRAY8);
    for (int x = 0; x < width; x++) {
        uint32_t lum16;
        if (gray) {
            lum16 = rc->in_lut[src[x]];
        } else {
            const uint8_t *p = src + x * 3;
            lum16 = (rc->wr * rc->in_lut[p[0]] + rc->wg * rc->in_lut[p[1]] +
                     rc->wb * rc->in_lut[p[2]]) >> 15;
        }
        /* 8-bit intensity: linear high byte (gamma deferred) or perceptual gray. */
        intensity[x] = rc->linear_downsample ? (uint8_t)(lum16 >> 8)
                                             : rc->out_lut[lum16 >> 8];
    }
}

void imgf_recolor_finalize(const imgf_recolor_t *rc, const uint8_t *intensity,
                           int width, uint8_t *dst) {
    for (int x = 0; x < width; x++) {
        uint8_t g = rc->linear_downsample ? rc->out_lut[intensity[x]] : intensity[x];
        dst[x] = rc->invert ? (uint8_t)(255 - g) : g;
    }
}
