/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Two-pass (horizontal then vertical) resizer with per-axis algorithm choice:
 * box-area average for downscale, bilinear for upscale, identity for equal.
 * Streaming model: caller pushes src rows in order, pops dst rows as they
 * become ready. Internally:
 *   - Each push unpacks the src row to 16-bit (irow), runs horizontal pass
 *     to produce hrow, then drives the vertical pass.
 *   - Vertical-down (box-area): accumulates hrow into vacc with overlap
 *     weights; finalizes the next dst row when its weight is full.
 *   - Vertical-up (bilinear): retains the previous hrow; produces dst rows
 *     by blending hrow_prev/hrow_curr at precomputed positions.
 * Cross pixfmt conversion is folded into the unpack/pack at the ends of the
 * pipeline (Gray<->RGB888<->RGB565 in any combination).
 */

#include "imgf_resize.h"

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_decoder.h"

#define Q16             65536
#define LUMA_R_Q8       54     /* BT.709 0.2126 * 256 */
#define LUMA_G_Q8       183
#define LUMA_B_Q8       19

struct imgf_resizer {
    int sw, sh, dw, dh;
    imgf_pixfmt_t src_pf, dst_pf;
    int internal_ch;
    int src_bpp, dst_bpp;

    /* horizontal mapping */
    bool h_up;
    int      *h_off;        /* dw+1 (downscale) */
    int      *h_src;        /* dw  (downscale) */
    uint32_t *h_w;          /* off[dw] (downscale) */
    uint32_t *h_wsum;       /* dw  (downscale) */
    int      *h_lo;         /* dw  (upscale) */
    int      *h_hi;         /* dw  (upscale) */
    uint32_t *h_blend;      /* dw  (upscale) Q16 weight on h_hi */

    /* vertical mapping (upscale only — downscale weights are computed
     * on-the-fly per push since they only depend on src_y/dst_y arithmetic) */
    bool v_up;
    int      *v_lo;         /* dh  (upscale) */
    int      *v_hi;         /* dh  (upscale) */
    uint32_t *v_blend;      /* dh  (upscale) */

    /* scratch (always present) */
    uint16_t *irow;         /* sw * internal_ch */
    uint16_t *hrow;         /* dw * internal_ch */

    /* upscale-v state */
    uint16_t *hrow_prev;    /* dw * internal_ch */
    uint16_t *blend_tmp;    /* dw * internal_ch (interp scratch before pack) */
    int       v_ready_max;  /* highest dst_y whose v_hi <= last pushed src_y; -1 initially */

    /* downscale-v state */
    uint64_t *vacc;         /* dw * internal_ch */
    uint64_t  vwsum;
    int       v_acc_dst_y;  /* next dst_y being accumulated */
    uint8_t  *pending;      /* dw * dst_bpp, packed and ready */
    uint8_t   pending_avail;

    /* common state */
    int src_y_next;         /* next src row to push (0..sh) */
    int dst_y_next;         /* next dst row to pop (0..dh) */
    uint8_t finished;       /* finish() called, no more pushes */

    imgf_err_t last_error;
};

/* ---- geometry --------------------------------------------------------- */

imgf_err_t imgf_resize_compute_dst(uint16_t src_w, uint16_t src_h,
                                   const imgf_resize_opts_t *opts,
                                   uint16_t *out_w, uint16_t *out_h) {
    if (!opts || src_w == 0 || src_h == 0) return IMGF_ERR_INVALID_ARG;
    int sw = src_w, sh = src_h;
    int tw = opts->target_w, th = opts->target_h;
    int dw, dh;
    if (opts->fit == IMGF_FIT_STRETCH) {
        if (tw <= 0 || th <= 0) return IMGF_ERR_INVALID_ARG;
        dw = tw; dh = th;
    } else {
        if (tw == 0 && th == 0) { dw = sw; dh = sh; }
        else {
            double rw = tw ? (double)tw / sw : 1e30;
            double rh = th ? (double)th / sh : 1e30;
            double r = rw < rh ? rw : rh;
            int nw = (int)lround(sw * r);
            int nh = (int)lround(sh * r);
            dw = nw < 1 ? 1 : nw;
            dh = nh < 1 ? 1 : nh;
        }
    }
    if (dw > 0xFFFF || dh > 0xFFFF) return IMGF_ERR_TOO_LARGE;
    if (out_w) *out_w = (uint16_t)dw;
    if (out_h) *out_h = (uint16_t)dh;
    return IMGF_OK;
}

/* ---- table builders --------------------------------------------------- */

static bool build_box_h(int s, int d, int **off_out, int **src_out,
                        uint32_t **w_out, uint32_t **wsum_out) {
    int *off = (int *)calloc((size_t)d + 1, sizeof(int));
    int *src = (int *)calloc((size_t)d, sizeof(int));
    uint32_t *wsum = (uint32_t *)calloc((size_t)d, sizeof(uint32_t));
    if (!off || !src || !wsum) goto fail;

    off[0] = 0;
    for (int x = 0; x < d; x++) {
        double fx0 = (double)x * s / d;
        double fx1 = (double)(x + 1) * s / d;
        int sx0 = (int)fx0;
        int sx1 = (int)ceil(fx1);
        if (sx0 < 0) sx0 = 0;
        if (sx1 > s) sx1 = s;
        if (sx1 <= sx0) sx1 = sx0 + 1;
        src[x] = sx0;
        off[x + 1] = off[x] + (sx1 - sx0);
    }
    uint32_t *w = (uint32_t *)calloc((size_t)off[d], sizeof(uint32_t));
    if (!w) goto fail;
    for (int x = 0; x < d; x++) {
        double fx0 = (double)x * s / d;
        double fx1 = (double)(x + 1) * s / d;
        int sx0 = src[x];
        int cnt = off[x + 1] - off[x];
        uint32_t ws = 0;
        for (int i = 0; i < cnt; i++) {
            double lo = fx0 > (double)(sx0 + i) ? fx0 : (double)(sx0 + i);
            double hi = fx1 < (double)(sx0 + i + 1) ? fx1 : (double)(sx0 + i + 1);
            uint32_t wi = (uint32_t)llround((hi - lo) * (double)Q16);
            if (!wi) wi = 1;
            w[off[x] + i] = wi;
            ws += wi;
        }
        wsum[x] = ws;
    }
    *off_out = off; *src_out = src; *w_out = w; *wsum_out = wsum;
    return true;
fail:
    free(off); free(src); free(wsum);
    return false;
}

static bool build_bilinear(int s, int d, int **lo_out, int **hi_out, uint32_t **blend_out) {
    int *lo = (int *)calloc((size_t)d, sizeof(int));
    int *hi = (int *)calloc((size_t)d, sizeof(int));
    uint32_t *blend = (uint32_t *)calloc((size_t)d, sizeof(uint32_t));
    if (!lo || !hi || !blend) { free(lo); free(hi); free(blend); return false; }

    for (int x = 0; x < d; x++) {
        double xs;
        if (s == 1) {
            xs = 0.0;
        } else if (d == 1) {
            xs = (double)(s - 1) * 0.5;
        } else {
            xs = (double)x * (s - 1) / (d - 1);
        }
        int xl = (int)floor(xs);
        if (xl < 0) xl = 0;
        if (xl > s - 1) xl = s - 1;
        int xh = xl + 1 < s ? xl + 1 : xl;
        double frac = xs - xl;
        if (frac < 0) frac = 0;
        if (frac > 1) frac = 1;
        lo[x] = xl;
        hi[x] = xh;
        blend[x] = (uint32_t)llround(frac * (double)Q16);
    }
    *lo_out = lo; *hi_out = hi; *blend_out = blend;
    return true;
}

/* ---- unpack / pack ---------------------------------------------------- */

static inline uint8_t clamp8(int v) {
    return v < 0 ? 0 : (v > 255 ? 255 : (uint8_t)v);
}

static void unpack_row(const uint8_t *src, uint16_t *dst, int w,
                       imgf_pixfmt_t src_pf, int ch) {
    if (ch == 1) {
        for (int i = 0; i < w; i++) dst[i] = src[i];
        return;
    }
    if (src_pf == IMGF_PIX_GRAY8) {
        for (int i = 0; i < w; i++) {
            uint16_t g = src[i];
            dst[3 * i + 0] = g;
            dst[3 * i + 1] = g;
            dst[3 * i + 2] = g;
        }
    } else if (src_pf == IMGF_PIX_RGB888) {
        for (int i = 0; i < w; i++) {
            dst[3 * i + 0] = src[3 * i + 0];
            dst[3 * i + 1] = src[3 * i + 1];
            dst[3 * i + 2] = src[3 * i + 2];
        }
    } else {  /* RGB565 (host-endian uint16) */
        const uint16_t *s = (const uint16_t *)src;
        for (int i = 0; i < w; i++) {
            uint16_t p = s[i];
            uint16_t r5 = (p >> 11) & 0x1F;
            uint16_t g6 = (p >> 5)  & 0x3F;
            uint16_t b5 =  p        & 0x1F;
            dst[3 * i + 0] = (uint16_t)((r5 << 3) | (r5 >> 2));
            dst[3 * i + 1] = (uint16_t)((g6 << 2) | (g6 >> 4));
            dst[3 * i + 2] = (uint16_t)((b5 << 3) | (b5 >> 2));
        }
    }
}

static void pack_row(const uint16_t *src, uint8_t *dst, int w,
                     imgf_pixfmt_t dst_pf, int ch) {
    if (ch == 1) {
        for (int i = 0; i < w; i++) dst[i] = clamp8((int)src[i]);
        return;
    }
    if (dst_pf == IMGF_PIX_GRAY8) {
        for (int i = 0; i < w; i++) {
            int r = src[3 * i + 0], g = src[3 * i + 1], b = src[3 * i + 2];
            int y = (r * LUMA_R_Q8 + g * LUMA_G_Q8 + b * LUMA_B_Q8 + 128) >> 8;
            dst[i] = clamp8(y);
        }
    } else if (dst_pf == IMGF_PIX_RGB888) {
        for (int i = 0; i < w; i++) {
            dst[3 * i + 0] = clamp8((int)src[3 * i + 0]);
            dst[3 * i + 1] = clamp8((int)src[3 * i + 1]);
            dst[3 * i + 2] = clamp8((int)src[3 * i + 2]);
        }
    } else {  /* RGB565 */
        uint16_t *d = (uint16_t *)dst;
        for (int i = 0; i < w; i++) {
            int r = clamp8((int)src[3 * i + 0]) >> 3;
            int g = clamp8((int)src[3 * i + 1]) >> 2;
            int b = clamp8((int)src[3 * i + 2]) >> 3;
            d[i] = (uint16_t)((r << 11) | (g << 5) | b);
        }
    }
}

/* ---- horizontal pass -------------------------------------------------- */

static void hreduce_box(const uint16_t *irow, int ch, int dw, uint16_t *hrow,
                        const int *off, const int *src, const uint32_t *w,
                        const uint32_t *wsum) {
    for (int x = 0; x < dw; x++) {
        const uint32_t *wp = w + off[x];
        const uint16_t *p  = irow + (size_t)src[x] * ch;
        int cnt = off[x + 1] - off[x];
        uint32_t ws = wsum[x];
        for (int c = 0; c < ch; c++) {
            uint64_t sum = 0;
            for (int i = 0; i < cnt; i++) sum += (uint64_t)p[i * ch + c] * wp[i];
            hrow[x * ch + c] = (uint16_t)(sum / ws);
        }
    }
}

static void hexpand_bilinear(const uint16_t *irow, int ch, int dw, uint16_t *hrow,
                             const int *lo, const int *hi, const uint32_t *blend) {
    for (int x = 0; x < dw; x++) {
        uint32_t b = blend[x];
        uint32_t inv = Q16 - b;
        const uint16_t *pl = irow + (size_t)lo[x] * ch;
        const uint16_t *ph = irow + (size_t)hi[x] * ch;
        for (int c = 0; c < ch; c++) {
            uint64_t v = (uint64_t)pl[c] * inv + (uint64_t)ph[c] * b;
            hrow[x * ch + c] = (uint16_t)(v >> 16);
        }
    }
}

/* ---- vertical pass ---------------------------------------------------- */

/* Downscale-v: contribute the just-computed hrow at src_y to dst_y_acc.. and
 * finalize whichever dst rows it completes. Returns 1 if a new dst row was
 * packed into r->pending (downscale produces <= 1 newly-ready per push). */
static int v_consume_box(imgf_resizer_t *r, int sy) {
    int dim = r->dw * r->internal_ch;
    double top = sy, bot = sy + 1.0;
    int produced = 0;
    while (r->v_acc_dst_y < r->dh) {
        int dy = r->v_acc_dst_y;
        double vy0 = (double)dy * r->sh / r->dh;
        double vy1 = (double)(dy + 1) * r->sh / r->dh;
        double overlap = (bot < vy1 ? bot : vy1) - (top > vy0 ? top : vy0);
        if (overlap > 0.0) {
            uint64_t wi = (uint64_t)llround(overlap * (double)Q16);
            if (!wi) wi = 1;
            for (int i = 0; i < dim; i++)
                r->vacc[i] += (uint64_t)r->hrow[i] * wi;
            r->vwsum += wi;
        }
        if (vy1 <= bot + 1e-9) {
            /* dst row dy fully covered — finalize */
            uint64_t inv = ((uint64_t)1 << 32) / r->vwsum;
            for (int i = 0; i < dim; i++) {
                uint32_t avg = (uint32_t)((r->vacc[i] * inv + ((uint64_t)1 << 31)) >> 32);
                if (avg > 65535) avg = 65535;
                r->hrow[i] = (uint16_t)avg;   /* reuse hrow as finalized-row scratch */
            }
            pack_row(r->hrow, r->pending, r->dw, r->dst_pf, r->internal_ch);
            r->pending_avail = 1;
            memset(r->vacc, 0, sizeof(uint64_t) * (size_t)dim);
            r->vwsum = 0;
            r->v_acc_dst_y = dy + 1;
            produced = 1;
            break;   /* downscale produces <= 1 newly-ready per src row */
        } else {
            break;   /* current dst row needs more src rows */
        }
    }
    return produced;
}

/* Upscale-v: advance v_ready_max so that dst rows up to that index become
 * available. Returns the number of dst rows that became newly ready by this
 * push. The actual blend + pack happens lazily in pop_row(). */
static int v_consume_bilinear_advance(imgf_resizer_t *r, int sy) {
    int newly = 0;
    while (r->v_ready_max + 1 < r->dh && r->v_hi[r->v_ready_max + 1] <= sy) {
        r->v_ready_max++;
        newly++;
    }
    return newly;
}

/* ---- create / destroy ------------------------------------------------- */

static void resizer_free(imgf_resizer_t *r) {
    if (!r) return;
    free(r->h_off);    free(r->h_src);    free(r->h_w);    free(r->h_wsum);
    free(r->h_lo);     free(r->h_hi);     free(r->h_blend);
    free(r->v_lo);     free(r->v_hi);     free(r->v_blend);
    free(r->irow);     free(r->hrow);
    free(r->hrow_prev); free(r->blend_tmp);
    free(r->vacc);     free(r->pending);
    free(r);
}

imgf_resizer_t *imgf_resizer_create(uint16_t src_w, uint16_t src_h,
                                    imgf_pixfmt_t src_pf,
                                    const imgf_resize_opts_t *opts,
                                    imgf_err_t *out_err) {
    static const imgf_resize_opts_t kZero = {0};
    if (!opts) opts = &kZero;

    if (src_w == 0 || src_h == 0 ||
        (src_pf != IMGF_PIX_GRAY8 && src_pf != IMGF_PIX_RGB888 && src_pf != IMGF_PIX_RGB565)) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }

    uint16_t dw16, dh16;
    imgf_err_t e = imgf_resize_compute_dst(src_w, src_h, opts, &dw16, &dh16);
    if (e != IMGF_OK) { if (out_err) *out_err = e; return NULL; }

    imgf_pixfmt_t dst_pf = opts->dst_pixfmt == IMGF_PIX_INHERIT ? src_pf : opts->dst_pixfmt;
    if (dst_pf != IMGF_PIX_GRAY8 && dst_pf != IMGF_PIX_RGB888 && dst_pf != IMGF_PIX_RGB565) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }

    imgf_resizer_t *r = (imgf_resizer_t *)calloc(1, sizeof *r);
    if (!r) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }

    r->sw = src_w; r->sh = src_h;
    r->dw = dw16;  r->dh = dh16;
    r->src_pf = src_pf;
    r->dst_pf = dst_pf;
    r->src_bpp = imgf_pixfmt_bpp(src_pf);
    r->dst_bpp = imgf_pixfmt_bpp(dst_pf);
    r->internal_ch = (src_pf == IMGF_PIX_GRAY8 && dst_pf == IMGF_PIX_GRAY8) ? 1 : 3;
    r->h_up = (r->dw > r->sw);
    r->v_up = (r->dh > r->sh);
    r->v_ready_max = -1;
    r->v_acc_dst_y = 0;

    size_t dim = (size_t)r->dw * r->internal_ch;
    r->irow = (uint16_t *)calloc((size_t)r->sw * r->internal_ch, sizeof(uint16_t));
    r->hrow = (uint16_t *)calloc(dim, sizeof(uint16_t));
    if (!r->irow || !r->hrow) goto oom;

    if (r->h_up) {
        if (!build_bilinear(r->sw, r->dw, &r->h_lo, &r->h_hi, &r->h_blend)) goto oom;
    } else {
        if (!build_box_h(r->sw, r->dw, &r->h_off, &r->h_src, &r->h_w, &r->h_wsum)) goto oom;
    }

    if (r->v_up) {
        if (!build_bilinear(r->sh, r->dh, &r->v_lo, &r->v_hi, &r->v_blend)) goto oom;
        r->hrow_prev = (uint16_t *)calloc(dim, sizeof(uint16_t));
        r->blend_tmp = (uint16_t *)calloc(dim, sizeof(uint16_t));
        if (!r->hrow_prev || !r->blend_tmp) goto oom;
    } else {
        r->vacc = (uint64_t *)calloc(dim, sizeof(uint64_t));
        r->pending = (uint8_t *)calloc((size_t)r->dw * r->dst_bpp, 1);
        if (!r->vacc || !r->pending) goto oom;
    }

    if (out_err) *out_err = IMGF_OK;
    return r;

oom:
    if (out_err) *out_err = IMGF_ERR_OOM;
    resizer_free(r);
    return NULL;
}

void imgf_resizer_destroy(imgf_resizer_t *r) { resizer_free(r); }

uint16_t      imgf_resizer_dst_width  (const imgf_resizer_t *r) { return r ? (uint16_t)r->dw : 0; }
uint16_t      imgf_resizer_dst_height (const imgf_resizer_t *r) { return r ? (uint16_t)r->dh : 0; }
imgf_pixfmt_t imgf_resizer_dst_pixfmt (const imgf_resizer_t *r) { return r ? r->dst_pf : IMGF_PIX_INHERIT; }
imgf_err_t    imgf_resizer_last_error (const imgf_resizer_t *r) { return r ? r->last_error : IMGF_ERR_INVALID_ARG; }

/* ---- push / pop ------------------------------------------------------- */

int imgf_resizer_push_row(imgf_resizer_t *r, const uint8_t *src_row) {
    if (!r || !src_row) return -1;
    if (r->finished || r->src_y_next >= r->sh) {
        r->last_error = IMGF_ERR_INVALID_STATE;
        return -1;
    }
    int sy = r->src_y_next;

    /* For upscale-v we keep two hrows; rotate before writing the new one. */
    if (r->v_up) {
        uint16_t *tmp = r->hrow_prev;
        r->hrow_prev = r->hrow;
        r->hrow = tmp;
    }

    unpack_row(src_row, r->irow, r->sw, r->src_pf, r->internal_ch);
    if (r->h_up) {
        hexpand_bilinear(r->irow, r->internal_ch, r->dw, r->hrow,
                         r->h_lo, r->h_hi, r->h_blend);
    } else {
        hreduce_box(r->irow, r->internal_ch, r->dw, r->hrow,
                    r->h_off, r->h_src, r->h_w, r->h_wsum);
    }

    int produced;
    if (r->v_up) {
        produced = v_consume_bilinear_advance(r, sy);
    } else {
        produced = v_consume_box(r, sy);
    }
    r->src_y_next = sy + 1;
    return produced;
}

bool imgf_resizer_pop_row(imgf_resizer_t *r, uint8_t *dst_row) {
    if (!r || !dst_row) return false;
    if (r->dst_y_next >= r->dh) return false;

    if (r->v_up) {
        if (r->dst_y_next > r->v_ready_max) return false;
        int dy = r->dst_y_next;
        int curr = r->src_y_next - 1;
        int lo = r->v_lo[dy], hi = r->v_hi[dy];
        uint32_t blend = r->v_blend[dy];
        const uint16_t *p_lo = (lo == curr) ? r->hrow : r->hrow_prev;
        const uint16_t *p_hi = (hi == curr) ? r->hrow : r->hrow_prev;
        int dim = r->dw * r->internal_ch;
        const uint16_t *src;
        if (lo == hi || blend == 0) {
            src = p_lo;
        } else if (blend >= Q16) {
            src = p_hi;
        } else {
            uint32_t inv = Q16 - blend;
            for (int i = 0; i < dim; i++) {
                uint64_t v = (uint64_t)p_lo[i] * inv + (uint64_t)p_hi[i] * blend;
                r->blend_tmp[i] = (uint16_t)((v + (1u << 15)) >> 16);
            }
            src = r->blend_tmp;
        }
        pack_row(src, dst_row, r->dw, r->dst_pf, r->internal_ch);
        r->dst_y_next = dy + 1;
        return true;
    }

    if (!r->pending_avail) return false;
    memcpy(dst_row, r->pending, (size_t)r->dw * r->dst_bpp);
    r->pending_avail = 0;
    r->dst_y_next++;
    return true;
}

int imgf_resizer_finish(imgf_resizer_t *r) {
    if (!r || r->finished) return 0;
    r->finished = 1;
    if (r->v_up) {
        /* For upscale-v with src_h > 1, the last dst row's v_hi == sh - 1, so
         * it became ready on the last push. Nothing to flush. */
        return 0;
    }
    /* Downscale-v: any residual partial dst row needs to be flushed. */
    if (r->v_acc_dst_y >= r->dh) return 0;
    if (r->vwsum == 0) return 0;
    int dim = r->dw * r->internal_ch;
    uint64_t inv = ((uint64_t)1 << 32) / r->vwsum;
    for (int i = 0; i < dim; i++) {
        uint32_t avg = (uint32_t)((r->vacc[i] * inv + ((uint64_t)1 << 31)) >> 32);
        if (avg > 65535) avg = 65535;
        r->hrow[i] = (uint16_t)avg;
    }
    pack_row(r->hrow, r->pending, r->dw, r->dst_pf, r->internal_ch);
    r->pending_avail = 1;
    r->v_acc_dst_y++;
    memset(r->vacc, 0, sizeof(uint64_t) * (size_t)dim);
    r->vwsum = 0;
    return 1;
}

/* ---- Layer 2 ---------------------------------------------------------- */

imgf_err_t imgf_resize_decoder(imgf_decoder_t *dec,
                               const imgf_resize_opts_t *opts,
                               uint8_t **out_data,
                               uint16_t *out_w, uint16_t *out_h) {
    if (!dec || !opts || !out_data) return IMGF_ERR_INVALID_ARG;

    uint16_t sw = imgf_decoder_width(dec);
    uint16_t sh = imgf_decoder_height(dec);
    imgf_pixfmt_t src_pf = imgf_decoder_pixfmt(dec);
    if (sw == 0 || sh == 0) return IMGF_ERR_INVALID_STATE;

    imgf_err_t err;
    imgf_resizer_t *r = imgf_resizer_create(sw, sh, src_pf, opts, &err);
    if (!r) return err;

    uint16_t dw = imgf_resizer_dst_width(r);
    uint16_t dh = imgf_resizer_dst_height(r);
    imgf_pixfmt_t dst_pf = imgf_resizer_dst_pixfmt(r);
    int dst_bpp = imgf_pixfmt_bpp(dst_pf);
    int src_bpp = imgf_pixfmt_bpp(src_pf);

    size_t buf_size = (size_t)dw * dh * dst_bpp;
    uint8_t *buf = (uint8_t *)imgf_alloc(buf_size, opts->alloc_caps);
    uint8_t *src_row = (uint8_t *)malloc((size_t)sw * src_bpp);
    if (!buf || !src_row) {
        if (buf) imgf_free(buf);
        free(src_row);
        imgf_resizer_destroy(r);
        return IMGF_ERR_OOM;
    }

    size_t row_stride = (size_t)dw * dst_bpp;
    int out_row = 0;

    for (uint16_t sy = 0; sy < sh; sy++) {
        if (!imgf_decoder_next_row(dec, src_row)) {
            imgf_err_t de = imgf_decoder_last_error(dec);
            free(src_row); imgf_free(buf); imgf_resizer_destroy(r);
            return de != IMGF_OK ? de : IMGF_ERR_TRUNCATED;
        }
        int ready = imgf_resizer_push_row(r, src_row);
        if (ready < 0) {
            imgf_err_t re = imgf_resizer_last_error(r);
            free(src_row); imgf_free(buf); imgf_resizer_destroy(r);
            return re;
        }
        for (int i = 0; i < ready && out_row < dh; i++) {
            imgf_resizer_pop_row(r, buf + (size_t)out_row * row_stride);
            out_row++;
        }
    }
    if (imgf_resizer_finish(r) > 0 && out_row < dh) {
        imgf_resizer_pop_row(r, buf + (size_t)out_row * row_stride);
        out_row++;
    }

    free(src_row);
    imgf_resizer_destroy(r);

    if (out_row != dh) { imgf_free(buf); return IMGF_ERR_DECODE; }

    *out_data = buf;
    if (out_w) *out_w = dw;
    if (out_h) *out_h = dh;
    return IMGF_OK;
}

imgf_err_t imgf_resize_buffer(const uint8_t *src, uint16_t src_w, uint16_t src_h,
                              size_t src_stride, imgf_pixfmt_t src_pf,
                              uint8_t *dst, size_t dst_stride,
                              const imgf_resize_opts_t *opts) {
    if (!src || !dst || !opts) return IMGF_ERR_INVALID_ARG;
    int src_bpp = imgf_pixfmt_bpp(src_pf);
    if (src_bpp == 0) return IMGF_ERR_INVALID_ARG;
    if (src_stride == 0) src_stride = (size_t)src_w * src_bpp;
    if (src_stride < (size_t)src_w * src_bpp) return IMGF_ERR_INVALID_ARG;

    uint16_t dw, dh;
    imgf_err_t e = imgf_resize_compute_dst(src_w, src_h, opts, &dw, &dh);
    if (e != IMGF_OK) return e;
    imgf_pixfmt_t dst_pf = opts->dst_pixfmt == IMGF_PIX_INHERIT ? src_pf : opts->dst_pixfmt;
    int dst_bpp = imgf_pixfmt_bpp(dst_pf);
    if (dst_bpp == 0) return IMGF_ERR_INVALID_ARG;
    size_t dst_row_bytes = (size_t)dw * dst_bpp;
    if (dst_stride == 0) dst_stride = dst_row_bytes;
    if (dst_stride < dst_row_bytes) return IMGF_ERR_INVALID_ARG;

    /* Overlap detection: when buffers intersect, upscale on any axis is
     * unsafe because the output footprint exceeds (per row or in total) the
     * area we've already consumed. */
    size_t src_size = (size_t)src_h * src_stride;
    size_t dst_size = (size_t)dh * dst_stride;
    bool overlap = (src <= dst && dst < src + src_size) ||
                   (dst <= src && src < dst + dst_size);
    if (overlap && (dw > src_w || dh > src_h)) return IMGF_ERR_INVALID_ARG;

    imgf_resizer_t *r = imgf_resizer_create(src_w, src_h, src_pf, opts, &e);
    if (!r) return e;

    int out_row = 0;
    for (uint16_t sy = 0; sy < src_h; sy++) {
        const uint8_t *srow = src + (size_t)sy * src_stride;
        int ready = imgf_resizer_push_row(r, srow);
        if (ready < 0) {
            imgf_err_t re = imgf_resizer_last_error(r);
            imgf_resizer_destroy(r);
            return re;
        }
        for (int i = 0; i < ready && out_row < dh; i++) {
            imgf_resizer_pop_row(r, dst + (size_t)out_row * dst_stride);
            out_row++;
        }
    }
    if (imgf_resizer_finish(r) > 0 && out_row < dh) {
        imgf_resizer_pop_row(r, dst + (size_t)out_row * dst_stride);
        out_row++;
    }
    imgf_resizer_destroy(r);
    if (out_row != dh) return IMGF_ERR_DECODE;
    return IMGF_OK;
}
