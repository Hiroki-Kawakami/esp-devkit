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

#include "imgf_alloc.h"
#include "imgf_resize.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_decoder.h"
#include "imgf_pie.h"

#define Q16             65536

/* Fractional bits the horizontal pass hands to a box vertical pass. Rounding
 * once per axis biases the result whenever both roundings land on a tie, which
 * an integer ratio hits constantly; carrying four bits through defers the only
 * rounding to the end. A bilinear vertical pass gets whole numbers, because it
 * passes rows through untouched and would have to undo the scale again. */
#define HROW_FRAC_BITS  4
#define LUMA_R_Q8       54     /* BT.709 0.2126 * 256 */
#define LUMA_G_Q8       183
#define LUMA_B_Q8       19

struct imgf_resizer {
    int sw, sh, dw, dh;
    imgf_pixfmt_t src_pf, dst_pf;
    int internal_ch;
    bool src_direct;        /* src rows are already the internal layout: skip irow */
    uint32_t caps;
    int src_bpp, dst_bpp;

    /* horizontal mapping */
    bool h_up;
    int      h_shift;       /* hrow = (weighted sum + h_round) >> h_shift */
    uint32_t h_round;
    int      *h_off;        /* dw+1 (downscale) */
    int      *h_src;        /* dw  (downscale) */
    uint32_t *h_w;          /* off[dw] (downscale); each dst pixel's run sums to Q16 */
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

    /* scratch a finished row is built in before pack_row. Never hrow: a src
     * row that straddles a dst boundary is still needed for the next dst row
     * after this one is packed. */
    uint16_t *vtmp;         /* dw * internal_ch */

    /* upscale-v state */
    uint16_t *hrow_prev;    /* dw * internal_ch */
    int       v_ready_max;  /* highest dst_y whose v_hi <= last pushed src_y; -1 initially */

    /* downscale-v state. The dst row being accumulated spans
     * [v_span_lo, v_span_hi) in units of 1/dh source rows, measured from the
     * start of the src row about to be pushed. v_cum is how much of that span
     * the pushes so far covered and v_wsum the Q16 weight it was charged, so
     * the weights of one dst row telescope to exactly Q16 however many source
     * rows it took. */
    uint32_t *vacc;         /* dw * internal_ch */
    int32_t   v_span_lo, v_span_hi;
    uint32_t  v_cum;
    uint32_t  v_wsum;
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
    } else if (tw == 0 && th == 0) {
        dw = sw; dh = sh;
    } else {
        /* The axis with the smaller target ratio sets the scale and keeps its
         * target exactly; the other is rounded to it. Every product here is
         * bounded by 65535 * 65535, so 32 bits are enough. */
        bool by_w = th == 0 || (tw != 0 && (uint32_t)tw * sh <= (uint32_t)th * sw);
        if (by_w) {
            dw = tw;
            dh = (int)(((uint32_t)sh * tw + sw / 2) / (uint32_t)sw);
        } else {
            dh = th;
            dw = (int)(((uint32_t)sw * th + sh / 2) / (uint32_t)sh);
        }
        if (dw < 1) dw = 1;
        if (dh < 1) dh = 1;
    }
    if (dw > 0xFFFF || dh > 0xFFFF) return IMGF_ERR_TOO_LARGE;
    if (out_w) *out_w = (uint16_t)dw;
    if (out_h) *out_h = (uint16_t)dh;
    return IMGF_OK;
}

/* ---- allocation ------------------------------------------------------- */

static void *rz_calloc(size_t count, size_t size, uint32_t caps) {
    const size_t bytes = count * size;
    void *p = imgf_alloc(bytes, caps);
    if (p) memset(p, 0, bytes);
    return p;
}

/* ---- table builders --------------------------------------------------- */

/* Box-area weights for one axis. Positions are kept in units of 1/d source
 * pixels so the whole table is integer, and each dst pixel's weights are the
 * differences of a Q16 prefix sum of its coverage: they add up to exactly Q16
 * whatever the tap count, which is what lets the row pass finish with a shift
 * instead of a divide. */
static bool build_box_h(int s, int d, int **off_out, int **src_out,
                        uint32_t **w_out, uint32_t caps) {
    int *off = (int *)rz_calloc((size_t)d + 1, sizeof(int), caps);
    int *src = (int *)rz_calloc((size_t)d, sizeof(int), caps);
    if (!off || !src) goto fail;

    off[0] = 0;
    for (int x = 0; x < d; x++) {
        int sx0 = (int)(((uint32_t)x * s) / (uint32_t)d);
        int sx1 = (int)(((uint32_t)(x + 1) * s + d - 1) / (uint32_t)d);
        if (sx1 > s) sx1 = s;
        if (sx1 <= sx0) sx1 = sx0 + 1;
        src[x] = sx0;
        off[x + 1] = off[x] + (sx1 - sx0);
    }
    uint32_t *w = (uint32_t *)rz_calloc((size_t)off[d], sizeof(uint32_t), caps);
    if (!w) goto fail;
    for (int x = 0; x < d; x++) {
        const uint32_t u0 = (uint32_t)x * s, u1 = (uint32_t)(x + 1) * s;
        const int sx0 = src[x], cnt = off[x + 1] - off[x];
        uint32_t cum = 0, wsum = 0;
        for (int i = 0; i < cnt; i++) {
            const uint32_t k0 = (uint32_t)(sx0 + i) * d, k1 = k0 + (uint32_t)d;
            const uint32_t lo = u0 > k0 ? u0 : k0;
            const uint32_t hi = u1 < k1 ? u1 : k1;
            if (hi > lo) cum += hi - lo;
            const uint32_t ws = (cum * Q16) / (uint32_t)s;
            w[off[x] + i] = ws - wsum;
            wsum = ws;
        }
    }
    *off_out = off; *src_out = src; *w_out = w;
    return true;
fail:
    imgf_free(off); imgf_free(src);
    return false;
}

static bool build_bilinear(int s, int d, int **lo_out, int **hi_out, uint32_t **blend_out,
                           uint32_t caps) {
    int *lo = (int *)rz_calloc((size_t)d, sizeof(int), caps);
    int *hi = (int *)rz_calloc((size_t)d, sizeof(int), caps);
    uint32_t *blend = (uint32_t *)rz_calloc((size_t)d, sizeof(uint32_t), caps);
    if (!lo || !hi || !blend) {
        imgf_free(lo); imgf_free(hi); imgf_free(blend);
        return false;
    }

    for (int x = 0; x < d; x++) {
        /* Source position of dst x, as num/den. */
        uint32_t num, den;
        if (s == 1)      { num = 0;                 den = 1; }
        else if (d == 1) { num = (uint32_t)(s - 1); den = 2; }
        else             { num = (uint32_t)x * (uint32_t)(s - 1); den = (uint32_t)(d - 1); }
        const int xl = (int)(num / den);
        const uint32_t rem = num % den;
        lo[x] = xl;
        hi[x] = xl + 1 < s ? xl + 1 : xl;
        blend[x] = (rem * Q16 + den / 2) / den;
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
    } else if (src_pf == IMGF_PIX_RGB888 || src_pf == IMGF_PIX_BGR888) {
        const int r = src_pf == IMGF_PIX_BGR888 ? 2 : 0;
        for (int i = 0; i < w; i++) {
            dst[3 * i + 0] = src[3 * i + r];
            dst[3 * i + 1] = src[3 * i + 1];
            dst[3 * i + 2] = src[3 * i + (2 - r)];
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
        int i = 0;
#if IMGF_HAVE_PIE
        if (dst_pf == IMGF_PIX_GRAY8 && w >= 16) {
            const int n = w >> 4;
            imgf_k_prepare();
            imgf_k_pack_bytes(dst, src, n);
            i = n << 4;
        }
#endif
        for (; i < w; i++) dst[i] = clamp8((int)src[i]);
        return;
    }
    if (dst_pf == IMGF_PIX_GRAY8) {
        for (int i = 0; i < w; i++) {
            int r = src[3 * i + 0], g = src[3 * i + 1], b = src[3 * i + 2];
            int y = (r * LUMA_R_Q8 + g * LUMA_G_Q8 + b * LUMA_B_Q8 + 128) >> 8;
            dst[i] = clamp8(y);
        }
    } else if (dst_pf == IMGF_PIX_RGB888 || dst_pf == IMGF_PIX_BGR888) {
        const int r = dst_pf == IMGF_PIX_BGR888 ? 2 : 0;
        int i = 0;
#if IMGF_HAVE_PIE
        if (r == 0 && w * 3 >= 16) {
            const int n = (w * 3) >> 4;
            imgf_k_prepare();
            imgf_k_pack_bytes(dst, src, n);
            i = (n << 4) / 3;
        }
#endif
        for (; i < w; i++) {
            dst[3 * i + r] = clamp8((int)src[3 * i + 0]);
            dst[3 * i + 1] = clamp8((int)src[3 * i + 1]);
            dst[3 * i + (2 - r)] = clamp8((int)src[3 * i + 2]);
        }
    } else {  /* RGB565 */
        uint16_t *d = (uint16_t *)dst;
        int i = 0;
#if IMGF_HAVE_PIE
        if (w >= 8) {
            const int n = w >> 3;
            imgf_k_prepare();
            imgf_k_pack_rgb565(dst, src, n);
            i = n << 3;
        }
#endif
        for (; i < w; i++) {
            int r = clamp8((int)src[3 * i + 0]) >> 3;
            int g = clamp8((int)src[3 * i + 1]) >> 2;
            int b = clamp8((int)src[3 * i + 2]) >> 3;
            d[i] = (uint16_t)((r << 11) | (g << 5) | b);
        }
    }
}

/* ---- horizontal pass -------------------------------------------------- */

/* The two-tap case is the whole of any reduction under 2x and most of the taps
 * of a larger one, so it is worth its own path: the general loop's bookkeeping
 * costs about as much as the arithmetic. HREDUCE_BOX is instantiated for the
 * 16-bit intermediate and, when the source rows already are the internal
 * layout, for the source bytes themselves. */
#define HREDUCE_BOX(name, type)                                                \
static void name(const type *irow, int ch, int dw, uint16_t *hrow,             \
                 const int *off, const int *src, const uint32_t *w,            \
                 int shift, uint32_t round) {                                  \
    for (int x = 0; x < dw; x++) {                                             \
        const uint32_t *wp = w + off[x];                                       \
        const type *p = irow + (size_t)src[x] * ch;                            \
        const int cnt = off[x + 1] - off[x];                                   \
        if (ch == 3) {                                                         \
            uint32_t s0, s1, s2;                                               \
            if (cnt == 2) {                                                    \
                const uint32_t w0 = wp[0], w1 = wp[1];                         \
                s0 = p[0] * w0 + p[3] * w1;                                    \
                s1 = p[1] * w0 + p[4] * w1;                                    \
                s2 = p[2] * w0 + p[5] * w1;                                    \
            } else {                                                           \
                s0 = s1 = s2 = 0;                                              \
                for (int i = 0; i < cnt; i++) {                                \
                    const uint32_t wi = wp[i];                                 \
                    s0 += p[0] * wi;                                           \
                    s1 += p[1] * wi;                                           \
                    s2 += p[2] * wi;                                           \
                    p += 3;                                                    \
                }                                                              \
            }                                                                  \
            hrow[x * 3 + 0] = (uint16_t)((s0 + round) >> shift);               \
            hrow[x * 3 + 1] = (uint16_t)((s1 + round) >> shift);               \
            hrow[x * 3 + 2] = (uint16_t)((s2 + round) >> shift);               \
        } else {                                                               \
            uint32_t s0 = 0;                                                   \
            for (int i = 0; i < cnt; i++) s0 += p[i] * wp[i];                  \
            hrow[x] = (uint16_t)((s0 + round) >> shift);                       \
        }                                                                      \
    }                                                                          \
}

HREDUCE_BOX(hreduce_box,    uint16_t)
HREDUCE_BOX(hreduce_box_u8, uint8_t)

#define HEXPAND_BILINEAR(name, type)                                           \
static void name(const type *irow, int ch, int dw, uint16_t *hrow,             \
                 const int *lo, const int *hi, const uint32_t *blend,          \
                 int shift, uint32_t round) {                                  \
    for (int x = 0; x < dw; x++) {                                             \
        const uint32_t b = blend[x], inv = Q16 - b;                            \
        const type *pl = irow + (size_t)lo[x] * ch;                            \
        const type *ph = irow + (size_t)hi[x] * ch;                            \
        for (int c = 0; c < ch; c++)                                           \
            hrow[x * ch + c] = (uint16_t)((pl[c] * inv + ph[c] * b + round) >> shift); \
    }                                                                          \
}

HEXPAND_BILINEAR(hexpand_bilinear,    uint16_t)
HEXPAND_BILINEAR(hexpand_bilinear_u8, uint8_t)

/* ---- vertical pass ---------------------------------------------------- */

/* Charge the part of the src row now in hrow that falls inside the dst row
 * being accumulated. The weight is the step of a Q16 prefix sum of the dst
 * row's coverage, so however many src rows it takes they add up to exactly
 * Q16 and the finalize below is a shift rather than a divide. */
static void v_charge(imgf_resizer_t *r, int dim) {
    const int32_t lo = r->v_span_lo > 0 ? r->v_span_lo : 0;
    const int32_t hi = r->v_span_hi < r->dh ? r->v_span_hi : r->dh;
    if (hi <= lo) return;
    r->v_cum += (uint32_t)(hi - lo);
    const uint32_t ws = (r->v_cum * Q16) / (uint32_t)r->sh;
    const uint32_t wi = ws - r->v_wsum;
    r->v_wsum = ws;
    for (int i = 0; i < dim; i++) r->vacc[i] += r->hrow[i] * wi;
}

#define VOUT_SHIFT (16 + HROW_FRAC_BITS)

static void v_finalize(imgf_resizer_t *r, int dim) {
    for (int i = 0; i < dim; i++)
        r->vtmp[i] = (uint16_t)((r->vacc[i] + (1u << (VOUT_SHIFT - 1))) >> VOUT_SHIFT);
    pack_row(r->vtmp, r->pending, r->dw, r->dst_pf, r->internal_ch);
    r->pending_avail = 1;
    memset(r->vacc, 0, sizeof(uint32_t) * (size_t)dim);
    r->v_acc_dst_y++;
    r->v_span_lo = r->v_span_hi;
    r->v_span_hi += r->sh;
    r->v_cum = 0;
    r->v_wsum = 0;
}

/* Downscale-v: give the just-computed hrow to the dst row being accumulated
 * and finalize that row once this src row covers its end. A src row that
 * straddles the boundary is split, so the second half starts the next dst row
 * straight away. Returns 1 if a dst row was packed into r->pending; since a
 * dst row is never shorter than a src row, at most one can complete per push.
 * src_y is implicit: the span is kept relative to it and slid one src row per
 * call. */
static int v_consume_box(imgf_resizer_t *r) {
    const int dim = r->dw * r->internal_ch;
    int produced = 0;

    if (r->v_acc_dst_y < r->dh) {
        v_charge(r, dim);
        if (r->v_span_hi <= r->dh) {
            v_finalize(r, dim);
            produced = 1;
            if (r->v_acc_dst_y < r->dh) v_charge(r, dim);
        }
    }
    r->v_span_lo -= r->dh;
    r->v_span_hi -= r->dh;
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
    imgf_free(r->h_off);     imgf_free(r->h_src);  imgf_free(r->h_w);
    imgf_free(r->h_lo);      imgf_free(r->h_hi);   imgf_free(r->h_blend);
    imgf_free(r->v_lo);      imgf_free(r->v_hi);   imgf_free(r->v_blend);
    imgf_free(r->irow);      imgf_free(r->hrow);      imgf_free(r->vtmp);
    imgf_free(r->hrow_prev);
    imgf_free(r->vacc);      imgf_free(r->pending);
    imgf_free(r);
}

imgf_resizer_t *imgf_resizer_create(uint16_t src_w, uint16_t src_h,
                                    imgf_pixfmt_t src_pf,
                                    const imgf_resize_opts_t *opts,
                                    imgf_err_t *out_err) {
    static const imgf_resize_opts_t kZero = {0};
    if (!opts) opts = &kZero;

    if (src_w == 0 || src_h == 0 ||
        (src_pf != IMGF_PIX_GRAY8 && src_pf != IMGF_PIX_RGB888 && src_pf != IMGF_PIX_RGB565 &&
         src_pf != IMGF_PIX_BGR888)) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }

    uint16_t dw16, dh16;
    imgf_err_t e = imgf_resize_compute_dst(src_w, src_h, opts, &dw16, &dh16);
    if (e != IMGF_OK) { if (out_err) *out_err = e; return NULL; }

    imgf_pixfmt_t dst_pf = opts->dst_pixfmt == IMGF_PIX_INHERIT ? src_pf : opts->dst_pixfmt;
    if (dst_pf != IMGF_PIX_GRAY8 && dst_pf != IMGF_PIX_RGB888 && dst_pf != IMGF_PIX_RGB565 &&
        dst_pf != IMGF_PIX_BGR888) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }

    imgf_resizer_t *r = (imgf_resizer_t *)rz_calloc(1, sizeof *r, opts->alloc_caps);
    if (!r) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
    r->caps = opts->alloc_caps;

    r->sw = src_w; r->sh = src_h;
    r->dw = dw16;  r->dh = dh16;
    r->src_pf = src_pf;
    r->dst_pf = dst_pf;
    r->src_bpp = imgf_pixfmt_bpp(src_pf);
    r->dst_bpp = imgf_pixfmt_bpp(dst_pf);
    r->internal_ch = (src_pf == IMGF_PIX_GRAY8 && dst_pf == IMGF_PIX_GRAY8) ? 1 : 3;
    /* The internal layout is R,G,B (or one grey channel), so these two source
     * formats are already it and the 16-bit unpack is pure overhead. */
    r->src_direct = (src_pf == IMGF_PIX_RGB888 && r->internal_ch == 3) ||
                    (src_pf == IMGF_PIX_GRAY8  && r->internal_ch == 1);
    r->h_up = (r->dw > r->sw);
    r->v_up = (r->dh > r->sh);
    r->h_shift = r->v_up ? 16 : 16 - HROW_FRAC_BITS;
    r->h_round = 1u << (r->h_shift - 1);
    r->v_ready_max = -1;
    r->v_acc_dst_y = 0;

    size_t dim = (size_t)r->dw * r->internal_ch;
    if (!r->src_direct) {
        r->irow = (uint16_t *)rz_calloc((size_t)r->sw * r->internal_ch,
                                        sizeof(uint16_t), r->caps);
        if (!r->irow) goto oom;
    }
    r->hrow = (uint16_t *)rz_calloc(dim, sizeof(uint16_t), r->caps);
    r->vtmp = (uint16_t *)rz_calloc(dim, sizeof(uint16_t), r->caps);
    if (!r->hrow || !r->vtmp) goto oom;

    if (r->h_up) {
        if (!build_bilinear(r->sw, r->dw, &r->h_lo, &r->h_hi, &r->h_blend, r->caps)) goto oom;
    } else {
        if (!build_box_h(r->sw, r->dw, &r->h_off, &r->h_src, &r->h_w, r->caps)) goto oom;
    }

    if (r->v_up) {
        if (!build_bilinear(r->sh, r->dh, &r->v_lo, &r->v_hi, &r->v_blend, r->caps)) goto oom;
        r->hrow_prev = (uint16_t *)rz_calloc(dim, sizeof(uint16_t), r->caps);
        if (!r->hrow_prev) goto oom;
    } else {
        r->vacc = (uint32_t *)rz_calloc(dim, sizeof(uint32_t), r->caps);
        r->v_span_hi = r->sh;
        r->pending = (uint8_t *)rz_calloc((size_t)r->dw * r->dst_bpp, 1, r->caps);
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

    if (r->src_direct) {
        if (r->h_up) {
            hexpand_bilinear_u8(src_row, r->internal_ch, r->dw, r->hrow,
                                r->h_lo, r->h_hi, r->h_blend, r->h_shift, r->h_round);
        } else {
            hreduce_box_u8(src_row, r->internal_ch, r->dw, r->hrow,
                           r->h_off, r->h_src, r->h_w, r->h_shift, r->h_round);
        }
    } else {
        unpack_row(src_row, r->irow, r->sw, r->src_pf, r->internal_ch);
        if (r->h_up) {
            hexpand_bilinear(r->irow, r->internal_ch, r->dw, r->hrow,
                             r->h_lo, r->h_hi, r->h_blend, r->h_shift, r->h_round);
        } else {
            hreduce_box(r->irow, r->internal_ch, r->dw, r->hrow,
                        r->h_off, r->h_src, r->h_w, r->h_shift, r->h_round);
        }
    }

    int produced;
    if (r->v_up) {
        produced = v_consume_bilinear_advance(r, sy);
    } else {
        produced = v_consume_box(r);
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
            for (int i = 0; i < dim; i++)
                r->vtmp[i] = (uint16_t)((p_lo[i] * inv + p_hi[i] * blend +
                                         Q16 / 2) >> 16);
            src = r->vtmp;
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
    /* Downscale-v: any residual partial dst row needs to be flushed. Its
     * weights stopped short of Q16, so this one row pays for a divide. */
    if (r->v_acc_dst_y >= r->dh) return 0;
    if (r->v_wsum == 0) return 0;
    int dim = r->dw * r->internal_ch;
    const uint32_t den = r->v_wsum << HROW_FRAC_BITS;
    for (int i = 0; i < dim; i++)
        r->vtmp[i] = (uint16_t)((r->vacc[i] + den / 2) / den);
    pack_row(r->vtmp, r->pending, r->dw, r->dst_pf, r->internal_ch);
    r->pending_avail = 1;
    r->v_acc_dst_y++;
    memset(r->vacc, 0, sizeof(uint32_t) * (size_t)dim);
    r->v_wsum = 0;
    r->v_cum = 0;
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
