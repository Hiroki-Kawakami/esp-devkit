/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Perceptual color management for the grayscale EPD path: linearize input
 * channels, weight to a configurable luminance, and re-encode to a perceptual
 * gray (sRGB / power / measured EPD reflectance LUT), with optional invert.
 * Richer than the resizer's fixed BT.709-in-gamma-space conversion.
 *
 * Two-phase streaming so the work straddles a box downscale and stays
 * photometrically correct:
 *   - to_intensity(): source row -> per-pixel intensity safe to area-average.
 *       linear_downsample=true  -> linear luminance (gamma deferred).
 *       linear_downsample=false -> perceptual gray (gamma applied here).
 *   - finalize(): averaged intensity -> final Gray8 (+ output gamma when
 *       deferred, + invert).
 * Feed Gray8 to a Gray8->Gray8 resizer between the two phases.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    IMGF_LUMA_REC709 = 0,   /* sRGB primaries (0.2126, 0.7152, 0.0722) */
    IMGF_LUMA_REC601,       /* JPEG YCbCr (0.299, 0.587, 0.114) */
    IMGF_LUMA_AVERAGE,      /* (R + G + B) / 3 */
    IMGF_LUMA_CUSTOM,       /* use opts.luma_custom */
} imgf_luma_t;

/* Electro-optical transfer of the input pixels (decode -> linear light). */
typedef enum {
    IMGF_GAMMA_IN_SRGB = 0, /* sRGB EOTF */
    IMGF_GAMMA_IN_POWER,    /* pure power curve, exponent = opts.in_power */
    IMGF_GAMMA_IN_LINEAR,   /* already linear */
} imgf_gamma_in_t;

/* Maps linear luminance -> perceptual gray that gets quantized/dithered. */
typedef enum {
    IMGF_GAMMA_OUT_SRGB = 0, /* sRGB OETF */
    IMGF_GAMMA_OUT_POWER,    /* pure power curve, exponent = opts.out_power */
    IMGF_GAMMA_OUT_EPDLUT,   /* opts.epd_lut16 (measured reflectance per level) */
} imgf_gamma_out_t;

typedef struct {
    imgf_luma_t      luma;
    float            luma_custom[3];     /* used when luma == CUSTOM */
    imgf_gamma_in_t  in_gamma;
    float            in_power;           /* used when in_gamma == POWER */
    imgf_gamma_out_t out_gamma;
    float            out_power;          /* used when out_gamma == POWER */
    const uint8_t   *epd_lut16;          /* 16 entries, used when out_gamma == EPDLUT */
    bool             linear_downsample;  /* average in linear light (defers out gamma) */
    bool             invert;             /* flip black/white in finalize */
} imgf_recolor_opts_t;

typedef struct imgf_recolor imgf_recolor_t;

/* `opts` may be NULL (all defaults: Rec709, sRGB in/out, linear_downsample). */
imgf_recolor_t *imgf_recolor_create (const imgf_recolor_opts_t *opts, imgf_err_t *out_err);
void            imgf_recolor_destroy(imgf_recolor_t *rc);

/* src_pf must be IMGF_PIX_GRAY8 or IMGF_PIX_RGB888. Writes `width` Gray8
 * intensities to `intensity` (the domain depends on linear_downsample). */
void imgf_recolor_to_intensity(const imgf_recolor_t *rc, const uint8_t *src,
                               int width, imgf_pixfmt_t src_pf, uint8_t *intensity);

/* Averaged intensity row -> final Gray8. dst may alias intensity (in-place). */
void imgf_recolor_finalize(const imgf_recolor_t *rc, const uint8_t *intensity,
                           int width, uint8_t *dst);

#ifdef __cplusplus
}
#endif
