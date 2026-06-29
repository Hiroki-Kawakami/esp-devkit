/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Quantize a Gray8 stream to N levels with a selectable method. Integer-only:
 * quantize is a 256-entry LUT, error diffusion carries (value << 8) fixed-point
 * error in int32 ring buffers; ordered (Bayer) is stateless. Ports the imgproc
 * Ditherer to C; same kernels and same arithmetic, so output is bit-identical.
 */

#include "imgf_dither.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"

/* ---- kernels ---------------------------------------------------------- */

typedef struct { int dx, dy, w; } diff_cell_t;
typedef struct { const diff_cell_t *cells; int count; int div; } diff_kernel_t;

static const diff_cell_t kFS[]       = {{1,0,7},{-1,1,3},{0,1,5},{1,1,1}};
static const diff_cell_t kAtkinson[] = {{1,0,1},{2,0,1},{-1,1,1},{0,1,1},{1,1,1},{0,2,1}};
static const diff_cell_t kSierra[]   = {{1,0,5},{2,0,3},
                                        {-2,1,2},{-1,1,4},{0,1,5},{1,1,4},{2,1,2},
                                        {-1,2,2},{0,2,3},{1,2,2}};
static const diff_cell_t kJJN[]      = {{1,0,7},{2,0,5},
                                        {-2,1,3},{-1,1,5},{0,1,7},{1,1,5},{2,1,3},
                                        {-2,2,1},{-1,2,3},{0,2,5},{1,2,3},{2,2,1}};
static const diff_cell_t kStucki[]   = {{1,0,8},{2,0,4},
                                        {-2,1,2},{-1,1,4},{0,1,8},{1,1,4},{2,1,2},
                                        {-2,2,1},{-1,2,2},{0,2,4},{1,2,2},{2,2,1}};

static diff_kernel_t kernel_for(imgf_dither_algo_t a) {
    switch (a) {
        case IMGF_DITHER_ATKINSON: return (diff_kernel_t){kAtkinson, 6, 8};
        case IMGF_DITHER_SIERRA:   return (diff_kernel_t){kSierra, 10, 32};
        case IMGF_DITHER_JJN:      return (diff_kernel_t){kJJN, 12, 48};
        case IMGF_DITHER_STUCKI:   return (diff_kernel_t){kStucki, 13, 42};
        case IMGF_DITHER_FLOYD_STEINBERG:
        default:                   return (diff_kernel_t){kFS, 4, 16};
    }
}

static const uint8_t kBayer2[4]  = {0, 2, 3, 1};
static const uint8_t kBayer4[16] = { 0, 8, 2,10, 12, 4,14, 6,
                                     3,11, 1, 9, 15, 7,13, 5};
static const uint8_t kBayer8[64] = {
     0,48,12,60, 3,51,15,63, 32,16,44,28,35,19,47,31,
     8,56, 4,52,11,59, 7,55, 40,24,36,20,43,27,39,23,
     2,50,14,62, 1,49,13,61, 34,18,46,30,33,17,45,29,
    10,58, 6,54, 9,57, 5,53, 42,26,38,22,41,25,37,21};

static void bayer_for(imgf_dither_algo_t a, const uint8_t **mat, int *size) {
    switch (a) {
        case IMGF_DITHER_BAYER2: *mat = kBayer2; *size = 2; break;
        case IMGF_DITHER_BAYER8: *mat = kBayer8; *size = 8; break;
        case IMGF_DITHER_BAYER4:
        default:                 *mat = kBayer4; *size = 4; break;
    }
}

/* ---- state ------------------------------------------------------------ */

struct imgf_dither {
    int                width;
    imgf_dither_algo_t algo;
    int                n;
    bool               serpentine;
    imgf_dither_out_t  out_mode;

    uint8_t qlut[256];    /* gray -> level (0..n-1) */
    uint8_t rlut[256];    /* level -> reconstructed gray (n entries used) */

    const uint8_t *bmat;
    int            bm;
    int            bthr[64];

    int32_t *err;         /* 3 * width fixed-point error (value << 8) */
    int      ring0;
    int      div;
    int      div_shift;   /* log2(div) if power of two; -1 otherwise */

    int      y;           /* next src row to push */
    uint8_t *pending;     /* width bytes, packed output of the last push */
    uint8_t  pending_avail;

    imgf_err_t last_error;
};

static int log2_pow2(int n) {
    int s = 0;
    while ((n & 1) == 0) { n >>= 1; s++; }
    return s;
}

/* ---- create / destroy ------------------------------------------------- */

imgf_dither_t *imgf_dither_create(uint16_t width,
                                  const imgf_dither_opts_t *opts,
                                  imgf_err_t *out_err) {
    static const imgf_dither_opts_t kZero = {0};
    if (!opts) opts = &kZero;
    if (width == 0) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    int n = opts->levels ? opts->levels : 16;
    if (n < 2) n = 2;

    imgf_dither_t *d = (imgf_dither_t *)calloc(1, sizeof *d);
    if (!d) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
    d->width      = width;
    d->algo       = opts->algo;
    d->n          = n;
    d->serpentine = opts->serpentine;
    d->out_mode   = opts->out_mode;

    /* quantize + reconstruct LUTs (match imgproc::Ditherer) */
    int diff = n - 1;
    for (int v = 0; v < 256; v++) {
        int level = (v * diff * 2 + 255) / 510;   /* round(v*(n-1)/255) */
        if (level > diff) level = diff;
        d->qlut[v] = (uint8_t)level;
    }
    for (int l = 0; l < n; l++) d->rlut[l] = (uint8_t)((l * 255 + diff / 2) / diff);

    switch (d->algo) {
        case IMGF_DITHER_NONE:
            break;
        case IMGF_DITHER_BAYER2:
        case IMGF_DITHER_BAYER4:
        case IMGF_DITHER_BAYER8: {
            bayer_for(d->algo, &d->bmat, &d->bm);
            int cells = d->bm * d->bm;
            for (int i = 0; i < cells; i++)
                d->bthr[i] = (255 * (2 * d->bmat[i] + 1) - 255 * cells) / (2 * cells);
            break;
        }
        default: {
            size_t err_bytes = (size_t)3 * width * sizeof(int32_t);
            d->err = (int32_t *)imgf_alloc(err_bytes, opts->alloc_caps);
            if (!d->err) { free(d); if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
            memset(d->err, 0, err_bytes);
            d->div = kernel_for(d->algo).div;
            d->div_shift = (d->div & (d->div - 1)) == 0 ? log2_pow2(d->div) : -1;
            break;
        }
    }

    d->pending = (uint8_t *)malloc(width);
    if (!d->pending) {
        if (d->err) imgf_free(d->err);
        free(d);
        if (out_err) *out_err = IMGF_ERR_OOM;
        return NULL;
    }

    if (out_err) *out_err = IMGF_OK;
    return d;
}

void imgf_dither_destroy(imgf_dither_t *d) {
    if (!d) return;
    if (d->err) imgf_free(d->err);
    free(d->pending);
    free(d);
}

uint8_t           imgf_dither_levels   (const imgf_dither_t *d) { return d ? (uint8_t)d->n : 0; }
imgf_dither_out_t imgf_dither_out_mode (const imgf_dither_t *d) { return d ? d->out_mode : IMGF_DITHER_OUT_INDEX; }
imgf_err_t        imgf_dither_last_error(const imgf_dither_t *d) { return d ? d->last_error : IMGF_ERR_INVALID_ARG; }

/* ---- per-row processors ----------------------------------------------- */

static inline uint8_t emit(const imgf_dither_t *d, int level) {
    return d->out_mode == IMGF_DITHER_OUT_GRAY ? d->rlut[level] : (uint8_t)level;
}

static void process_none(imgf_dither_t *d, const uint8_t *gray) {
    for (int x = 0; x < d->width; x++) d->pending[x] = emit(d, d->qlut[gray[x]]);
}

static void process_bayer(imgf_dither_t *d, const uint8_t *gray) {
    int diff = d->n - 1;
    int yr = d->y % d->bm;
    for (int x = 0; x < d->width; x++) {
        int num = gray[x] * diff + d->bthr[yr * d->bm + (x % d->bm)];
        int level = num <= 0 ? 0 : (num * 2 + 255) / 510;
        if (level > diff) level = diff;
        d->pending[x] = emit(d, level);
    }
}

static void process_diffuse(imgf_dither_t *d, const uint8_t *gray) {
    int32_t *cur = d->err + (size_t)d->ring0 * d->width;
    int32_t *n1  = d->err + (size_t)((d->ring0 + 1) % 3) * d->width;
    int32_t *n2  = d->err + (size_t)((d->ring0 + 2) % 3) * d->width;

    diff_kernel_t k = kernel_for(d->algo);
    int dir = (d->serpentine && (d->y & 1)) ? -1 : 1;
    int xstart = dir > 0 ? 0 : d->width - 1;
    int xend   = dir > 0 ? d->width : -1;

    for (int x = xstart; x != xend; x += dir) {
        int vf = (gray[x] << 8) + cur[x];
        if (vf < 0) vf = 0;
        else if (vf > (255 << 8)) vf = 255 << 8;
        int level = d->qlut[(vf + 128) >> 8];
        d->pending[x] = emit(d, level);
        int ef = vf - (d->rlut[level] << 8);
        for (int i = 0; i < k.count; i++) {
            int tx = x + dir * k.cells[i].dx;
            if (tx < 0 || tx >= d->width) continue;
            int wn = ef * k.cells[i].w;
            int add = d->div_shift >= 0 ? (wn >> d->div_shift) : (wn / d->div);
            switch (k.cells[i].dy) {
                case 0: cur[tx] += add; break;
                case 1: n1[tx]  += add; break;
                default: n2[tx] += add; break;
            }
        }
    }

    memset(cur, 0, sizeof(int32_t) * (size_t)d->width);
    d->ring0 = (d->ring0 + 1) % 3;
}

/* ---- push / pop ------------------------------------------------------- */

int imgf_dither_push_row(imgf_dither_t *d, const uint8_t *gray_row) {
    if (!d || !gray_row) return -1;
    if (d->pending_avail) {
        d->last_error = IMGF_ERR_INVALID_STATE;
        return -1;
    }
    switch (d->algo) {
        case IMGF_DITHER_NONE:    process_none(d, gray_row); break;
        case IMGF_DITHER_BAYER2:
        case IMGF_DITHER_BAYER4:
        case IMGF_DITHER_BAYER8:  process_bayer(d, gray_row); break;
        default:                  process_diffuse(d, gray_row); break;
    }
    d->pending_avail = 1;
    d->y++;
    return 1;
}

bool imgf_dither_pop_row(imgf_dither_t *d, uint8_t *out_row) {
    if (!d || !out_row || !d->pending_avail) return false;
    memcpy(out_row, d->pending, (size_t)d->width);
    d->pending_avail = 0;
    return true;
}

/* ---- Layer 2: buffer helper ------------------------------------------- */

imgf_err_t imgf_dither_buffer(const uint8_t *src, uint16_t w, uint16_t h,
                              size_t src_stride,
                              uint8_t *dst, size_t dst_stride,
                              const imgf_dither_opts_t *opts) {
    if (!src || !dst || w == 0 || h == 0) return IMGF_ERR_INVALID_ARG;
    if (src_stride == 0) src_stride = w;
    if (dst_stride == 0) dst_stride = w;
    if (src_stride < (size_t)w || dst_stride < (size_t)w) return IMGF_ERR_INVALID_ARG;

    imgf_err_t err;
    imgf_dither_t *d = imgf_dither_create(w, opts, &err);
    if (!d) return err;

    for (uint16_t y = 0; y < h; y++) {
        if (imgf_dither_push_row(d, src + (size_t)y * src_stride) < 0) {
            err = imgf_dither_last_error(d);
            imgf_dither_destroy(d);
            return err;
        }
        imgf_dither_pop_row(d, dst + (size_t)y * dst_stride);
    }
    imgf_dither_destroy(d);
    return IMGF_OK;
}
