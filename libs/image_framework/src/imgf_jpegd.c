/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * JPEG decoder. Pulls bytes from imgf_breader_t and serves rows top-to-bottom;
 * decodes one MCU row at a time with a separable matrix IDCT and per-block
 * sub-sampled IDCT (1/N for N in 2..8). The IDCT writes straight into a band
 * of planar component samples; chroma upsampling and the colour conversion
 * then run once per requested row, over the whole row at once.
 *
 * Baseline streams one scan and never holds more than a band. Progressive
 * streams cannot: every scan refines the whole frame, so the coefficients of
 * the whole image are decoded into per-component arrays on the first row
 * request and the band is then filled from them, one MCU row at a time.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_breader.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"
#include "imgf_jpegd.h"
#include "imgf_pie.h"

#define IMGF_JPEG_FAST_BITS 9
#define IMGF_JPEG_FAST_SIZE (1 << IMGF_JPEG_FAST_BITS)

typedef struct {
    uint8_t bits[17];
    uint8_t vals[256];
    int     mincode[17];
    int     maxcode[17];
    int     valptr[17];
    uint16_t fast[IMGF_JPEG_FAST_SIZE];
    uint8_t  defined;
} jpegd_huff_t;

typedef struct {
    int id, h, v, tq;
    int td, ta;
    int dcpred;
    int bw, bh;      /* coefficient grid, padded to whole MCUs */
    int nbw, nbh;    /* blocks that actually cover the image */
    int16_t **rows;  /* progressive only: one bw-block row per entry */
} jpegd_comp_t;

typedef struct {
    imgf_decoder_t base;

    imgf_breader_t br;
    uint8_t        br_ready;

    uint32_t alloc_caps;

    uint16_t w, h;
    int      ncomp;
    jpegd_comp_t comp[3];
    int      hmax, vmax;
    int      out_ch;

    uint16_t qt[4][64];
    uint16_t dqt[4][64];   /* quant tables in natural order */
    jpegd_huff_t dc[4];
    jpegd_huff_t ac[4];
    int      restart_interval;

    uint8_t  progressive;
    uint8_t  prog_ready;
    int      scan_ncomp;
    int      scan_comp[3];
    int      ss, se, ah, al;
    int      eobrun;

    int      scale;
    int      blk;
    int      mcus_per_row;
    int      mcu_rows;
    int      mcu_row_idx;

    uint8_t *band;            /* one allocation, carved into plane[] */
    uint8_t *plane[3];
    int      pstride[3];
    int      vsh[3], hsh[3];  /* band row/column -> plane row/column shift */
    int      band_h;
    int      band_valid_rows;
    int      band_row;
    uint32_t out_row;

    uint32_t bitbuf;
    int      bitcnt;
    uint8_t  marker_pending;
    int      marker;
    uint32_t mcu_count;

    uint8_t  err;
} jpegd_t;

static const uint8_t kZigzag[64] = {
     0,  1,  8, 16,  9,  2,  3, 10, 17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63};

/* Separable IDCT matrix, Q14: kIdctM[u][x] = c_u cos((2x+1)u.pi/16) / 2, so
 * the two passes carry the 1/4 of the 2-D transform between them. A row's
 * absolute sum is 43284, which keeps a 32-bit accumulator exact even when
 * every input has saturated. */
static const int16_t kIdctM[8][8] = {
    {  5793,   5793,   5793,   5793,   5793,   5793,   5793,   5793},
    {  8035,   6811,   4551,   1598,  -1598,  -4551,  -6811,  -8035},
    {  7568,   3135,  -3135,  -7568,  -7568,  -3135,   3135,   7568},
    {  6811,  -1598,  -8035,  -4551,   4551,   8035,   1598,  -6811},
    {  5793,  -5793,  -5793,   5793,   5793,  -5793,  -5793,   5793},
    {  4551,  -8035,   1598,   6811,  -6811,  -1598,   8035,  -4551},
    {  3135,  -7568,   7568,  -3135,  -3135,   7568,  -7568,   3135},
    {  1598,  -4551,   6811,  -8035,   8035,  -6811,   4551,  -1598},
};

/* The row pass keeps two fractional bits, which is what an integer
 * intermediate costs in accuracy otherwise. */
/* Matrices for the sub-sampled outputs: row u is the full block's basis
 * function for frequency u, averaged over each output sample's 8/N inputs. All
 * eight frequencies are kept, so the result is the box average of the full
 * block rather than a truncation of it. */
static const int16_t kIdctA4[8 * 4] = {
      5793,   5793,   5793,   5793,
      7423,   3075,  -3075,  -7423,
      5352,  -5352,  -5352,   5352,
      2607,  -6293,   6293,  -2607,
         0,      0,      0,      0,
     -1742,   4205,  -4205,   1742,
     -2217,   2217,   2217,  -2217,
     -1477,   -612,    612,   1477,
};
static const int16_t kIdctA2[8 * 2] = {
      5793,   5793,
      5249,  -5249,
         0,      0,
     -1843,   1843,
         0,      0,
      1232,  -1232,
         0,      0,
     -1044,   1044,
};

#define IDCT_PASS1_BITS 12
#define IDCT_PASS2_BITS 16

static inline uint8_t clamp8(int v) {
    return v < 0 ? 0 : (v > 255 ? 255 : (uint8_t)v);
}

static inline int16_t sat16(int v) {
    return v < -32768 ? (int16_t)-32768 : (v > 32767 ? (int16_t)32767 : (int16_t)v);
}

#if !IMGF_HAVE_PIE
/* Spatial block from dequantized coefficients in natural order. rowmask bit v
 * is set when row v has a non-zero coefficient; a clear bit contributes
 * nothing, so skipping it is exact. */
static void idct8x8(const int16_t *dqc, unsigned rowmask, int16_t *out) {
    for (int v = 0; v < 8; v++) {
        if (!(rowmask & (1u << v))) {
            memset(out + v * 8, 0, 8 * sizeof(int16_t));
            continue;
        }
        const int16_t *in = dqc + v * 8;
        for (int x = 0; x < 8; x++) {
            int acc = 1 << (IDCT_PASS1_BITS - 1);
            for (int u = 0; u < 8; u++) acc += in[u] * kIdctM[u][x];
            out[v * 8 + x] = sat16(acc >> IDCT_PASS1_BITS);
        }
    }
    int16_t col[8];
    for (int x = 0; x < 8; x++) {
        for (int v = 0; v < 8; v++) col[v] = out[v * 8 + x];
        for (int y = 0; y < 8; y++) {
            int acc = 1 << (IDCT_PASS2_BITS - 1);
            for (int v = 0; v < 8; v++) acc += col[v] * kIdctM[v][y];
            out[y * 8 + x] = sat16(acc >> IDCT_PASS2_BITS);
        }
    }
}

#endif

static void idct8x8_u8(const int16_t *dqc, unsigned rowmask,
                       uint8_t *dst, int stride) {
#if IMGF_HAVE_PIE
    imgf_k_prepare();
    imgf_k_idct(dst, stride, dqc, (int)rowmask, imgf_idct_tab8);
#else
    int16_t sp[64];
    idct8x8(dqc, rowmask, sp);
    for (int y = 0; y < 8; y++)
        for (int x = 0; x < 8; x++)
            dst[y * stride + x] = clamp8(sp[y * 8 + x] + 128);
#endif
}

/* n x n output, n in {2, 4}. Both passes run over all eight frequencies. */
static void idct_nxn(const int16_t *dqc, unsigned rowmask, int n, const int16_t *a,
                     uint8_t *dst, int stride) {
#if IMGF_HAVE_PIE
    (void)a;
    uint8_t sp[64] __attribute__((aligned(16)));
    imgf_k_prepare();
    imgf_k_idct(sp, 8, dqc, (int)rowmask, n == 4 ? imgf_idct_tab4 : imgf_idct_tab2);
    for (int y = 0; y < n; y++) memcpy(dst + (size_t)y * stride, sp + y * 8, (size_t)n);
#else
    int16_t t[8 * 4];
    for (int v = 0; v < 8; v++) {
        if (!(rowmask & (1u << v))) {
            for (int x = 0; x < n; x++) t[v * n + x] = 0;
            continue;
        }
        const int16_t *in = dqc + v * 8;
        for (int x = 0; x < n; x++) {
            int acc = 1 << (IDCT_PASS1_BITS - 1);
            for (int u = 0; u < 8; u++) acc += in[u] * a[u * n + x];
            t[v * n + x] = sat16(acc >> IDCT_PASS1_BITS);
        }
    }
    for (int y = 0; y < n; y++) {
        for (int x = 0; x < n; x++) {
            int acc = 1 << (IDCT_PASS2_BITS - 1);
            for (int v = 0; v < 8; v++) acc += t[v * n + x] * a[v * n + y];
            dst[y * stride + x] = clamp8(sat16(acc >> IDCT_PASS2_BITS) + 128);
        }
    }
#endif
}

/* The DC-only value, through the same two roundings as a full block so the
 * shortcut cannot disagree with it. */
static uint8_t idct_dc(int dc) {
    int t = sat16((dc * kIdctM[0][0] + (1 << (IDCT_PASS1_BITS - 1))) >> IDCT_PASS1_BITS);
    return clamp8(sat16((t * kIdctM[0][0] + (1 << (IDCT_PASS2_BITS - 1))) >> IDCT_PASS2_BITS)
                  + 128);
}

/* --- header parsing -------------------------------------------------------- */

static int raw_byte(jpegd_t *d) {
    return imgf_breader_byte(&d->br);
}

static int u16(jpegd_t *d) {
    int hi = raw_byte(d), lo = raw_byte(d);
    if (hi < 0 || lo < 0) return -1;
    return (hi << 8) | lo;
}

static bool skip_bytes(jpegd_t *d, int n) {
    while (n-- > 0)
        if (raw_byte(d) < 0) return false;
    return true;
}

static bool parse_dqt(jpegd_t *d, int len) {
    while (len > 0) {
        int pq_tq = raw_byte(d);
        if (pq_tq < 0) return false;
        int pq = pq_tq >> 4, tq = pq_tq & 0x0F;
        if (tq > 3) return false;
        len -= 1;
        for (int k = 0; k < 64; k++) {
            int v = pq ? u16(d) : raw_byte(d);
            if (v < 0) return false;
            d->qt[tq][k] = (uint16_t)v;
        }
        len -= pq ? 128 : 64;
    }
    return true;
}

static bool parse_dht(jpegd_t *d, int len) {
    while (len > 0) {
        int tc_th = raw_byte(d);
        if (tc_th < 0) return false;
        int tc = tc_th >> 4, th = tc_th & 0x0F;
        if (tc > 1 || th > 3) return false;
        jpegd_huff_t *t = tc ? &d->ac[th] : &d->dc[th];
        int total = 0;
        t->bits[0] = 0;
        for (int i = 1; i <= 16; i++) {
            int c = raw_byte(d);
            if (c < 0) return false;
            t->bits[i] = (uint8_t)c;
            total += c;
        }
        if (total > 256) return false;
        for (int i = 0; i < total; i++) {
            int v = raw_byte(d);
            if (v < 0) return false;
            t->vals[i] = (uint8_t)v;
        }
        int code = 0, k = 0;
        for (int l = 1; l <= 16; l++) {
            if (t->bits[l]) {
                t->valptr[l]  = k;
                t->mincode[l] = code;
                code += t->bits[l];
                /* An over-subscribed table has no valid codes left at this
                   length; its mincode would index past the fast table. */
                if (code > (1 << l)) return false;
                t->maxcode[l] = code - 1;
                k += t->bits[l];
            } else {
                t->maxcode[l] = -1;
            }
            code <<= 1;
        }
        for (int i = 0; i < IMGF_JPEG_FAST_SIZE; i++) t->fast[i] = 0;
        for (int l = 1; l <= IMGF_JPEG_FAST_BITS; l++) {
            for (int i = 0; i < t->bits[l]; i++) {
                int base = (t->mincode[l] + i) << (IMGF_JPEG_FAST_BITS - l);
                int cnt = 1 << (IMGF_JPEG_FAST_BITS - l);
                uint16_t entry = (uint16_t)((l << 8) | t->vals[t->valptr[l] + i]);
                for (int j = 0; j < cnt; j++) t->fast[base + j] = entry;
            }
        }
        t->defined = 1;
        len -= 17 + total;
    }
    return true;
}

static imgf_err_t parse_sof(jpegd_t *d, int len, int progressive) {
    (void)len;
    d->progressive = (uint8_t)progressive;
    int prec = raw_byte(d);
    if (prec != 8) return IMGF_ERR_UNSUPPORTED;
    int h = u16(d), w = u16(d);
    int nc = raw_byte(d);
    if (h <= 0 || w <= 0) return IMGF_ERR_DECODE;
    if (nc != 1 && nc != 3) return IMGF_ERR_UNSUPPORTED;
    d->h = (uint16_t)h;
    d->w = (uint16_t)w;
    d->ncomp = nc;
    d->hmax = d->vmax = 1;
    for (int i = 0; i < nc; i++) {
        int id = raw_byte(d), hv = raw_byte(d), tq = raw_byte(d);
        if (id < 0 || hv < 0 || tq < 0 || tq > 3) return IMGF_ERR_DECODE;
        d->comp[i].id = id;
        d->comp[i].h  = hv >> 4;
        d->comp[i].v  = hv & 0x0F;
        d->comp[i].tq = tq;
        if (d->comp[i].h < 1 || d->comp[i].h > 2 || d->comp[i].v < 1 || d->comp[i].v > 2)
            return IMGF_ERR_UNSUPPORTED;
        if (d->comp[i].h > d->hmax) d->hmax = d->comp[i].h;
        if (d->comp[i].v > d->vmax) d->vmax = d->comp[i].v;
    }
    return IMGF_OK;
}

static imgf_err_t parse_sos(jpegd_t *d, int len) {
    (void)len;
    int ns = raw_byte(d);
    if (ns < 1 || ns > d->ncomp) return IMGF_ERR_DECODE;
    if (!d->progressive && ns != d->ncomp) return IMGF_ERR_UNSUPPORTED;
    d->scan_ncomp = ns;
    for (int i = 0; i < ns; i++) {
        int cs = raw_byte(d), td_ta = raw_byte(d);
        if (cs < 0 || td_ta < 0) return IMGF_ERR_DECODE;
        int ci = -1;
        for (int j = 0; j < d->ncomp; j++)
            if (d->comp[j].id == cs) ci = j;
        if (ci < 0) return IMGF_ERR_DECODE;
        d->scan_comp[i] = ci;
        d->comp[ci].td = td_ta >> 4;
        d->comp[ci].ta = td_ta & 0x0F;
    }
    int ss = raw_byte(d), se = raw_byte(d), ahal = raw_byte(d);
    if (ss < 0 || se < 0 || ahal < 0) return IMGF_ERR_DECODE;
    d->ss = ss;
    d->se = se;
    d->ah = ahal >> 4;
    d->al = ahal & 0x0F;
    if (d->progressive) {
        if (d->se > 63 || d->ss > d->se) return IMGF_ERR_DECODE;
        if (d->ss == 0 && d->se != 0) return IMGF_ERR_DECODE;
        if (d->ss != 0 && ns != 1) return IMGF_ERR_DECODE;
        if (d->ah > 13 || d->al > 13) return IMGF_ERR_DECODE;
    }
    /* A progressive scan only reads the table its spectral range asks for, and
       encoders leave junk in the other selector. */
    bool need_dc = !d->progressive || d->ss == 0;
    bool need_ac = !d->progressive || d->se != 0;
    for (int i = 0; i < ns; i++) {
        const jpegd_comp_t *c = &d->comp[d->scan_comp[i]];
        if ((need_dc && c->td > 3) || (need_ac && c->ta > 3)) return IMGF_ERR_DECODE;
    }
    return IMGF_OK;
}

/* Quantization tables in natural order. Progressive streams may redefine one
 * between scans, so this runs again once every scan has been read. */
static void build_dq_tables(jpegd_t *d) {
    for (int i = 0; i < d->ncomp; i++) {
        int tq = d->comp[i].tq;
        for (int k = 0; k < 64; k++) d->dqt[tq][kZigzag[k]] = d->qt[tq][k];
    }
}

static imgf_err_t setup(jpegd_t *d, const imgf_decode_opts_t *opts) {
    d->scale = 1;
    if (opts->target_w > 0 && opts->target_h > 0) {
        while (d->scale < 8 &&
               (d->w / (d->scale * 2)) >= opts->target_w &&
               (d->h / (d->scale * 2)) >= opts->target_h)
            d->scale *= 2;
    }
    if (opts->max_src_pixels) {
        while (d->scale < 8) {
            uint32_t dw = (d->w + d->scale - 1) / d->scale;
            uint32_t dh = (d->h + d->scale - 1) / d->scale;
            if ((uint64_t)dw * dh <= opts->max_src_pixels) break;
            d->scale *= 2;
        }
    }
    d->blk = 8 / d->scale;

    d->mcus_per_row = (d->w + 8 * d->hmax - 1) / (8 * d->hmax);
    d->mcu_rows     = (d->h + 8 * d->vmax - 1) / (8 * d->vmax);

    for (int i = 0; i < d->ncomp; i++) {
        jpegd_comp_t *c = &d->comp[i];
        c->bw  = d->mcus_per_row * c->h;
        c->bh  = d->mcu_rows * c->v;
        c->nbw = ((d->w * c->h + d->hmax - 1) / d->hmax + 7) / 8;
        c->nbh = ((d->h * c->v + d->vmax - 1) / d->vmax + 7) / 8;
    }

    d->base.width  = (uint16_t)((d->w + d->scale - 1) / d->scale);
    d->base.height = (uint16_t)((d->h + d->scale - 1) / d->scale);
    d->out_ch      = d->ncomp == 1 ? 1 : 3;
    d->base.pixfmt = d->ncomp == 1 ? IMGF_PIX_GRAY8 : IMGF_PIX_RGB888;

    d->band_h = d->blk * d->vmax;

    size_t band_bytes = 0;
    for (int i = 0; i < d->ncomp; i++) {
        const jpegd_comp_t *c = &d->comp[i];
        d->vsh[i]     = c->v == d->vmax ? 0 : 1;
        d->hsh[i]     = c->h == d->hmax ? 0 : 1;
        d->pstride[i] = d->mcus_per_row * c->h * d->blk;
        band_bytes += (size_t)d->pstride[i] * (c->v * d->blk);
    }
    band_bytes += 16;   /* the SIMD colour conversion reads past the last row */
    /* Internal RAM is the fast place for the band, but only when the caller
       left the choice open: an explicit alloc_caps is a memory budget. */
    d->band = NULL;
    if (!d->alloc_caps && band_bytes <= 96 * 1024) {
        d->band = (uint8_t *)imgf_alloc_internal(band_bytes);
    }
    if (!d->band) d->band = (uint8_t *)imgf_alloc(band_bytes, d->alloc_caps);
    if (!d->band) return IMGF_ERR_OOM;
    {
        uint8_t *p = d->band;
        for (int i = 0; i < d->ncomp; i++) {
            d->plane[i] = p;
            p += (size_t)d->pstride[i] * (d->comp[i].v * d->blk);
        }
    }

    for (int i = 0; i < d->ncomp; i++) d->comp[i].dcpred = 0;

    build_dq_tables(d);
    return IMGF_OK;
}

static imgf_err_t parse_header(jpegd_t *d, const imgf_decode_opts_t *opts) {
    if (raw_byte(d) != 0xFF || raw_byte(d) != 0xD8) return IMGF_ERR_DECODE;

    bool have_sof = false;
    for (;;) {
        int b = raw_byte(d);
        if (b < 0) return IMGF_ERR_TRUNCATED;
        if (b != 0xFF) continue;
        int marker = raw_byte(d);
        while (marker == 0xFF) marker = raw_byte(d);
        if (marker < 0) return IMGF_ERR_TRUNCATED;
        if (marker == 0xD9) return IMGF_ERR_DECODE;
        if (marker >= 0xD0 && marker <= 0xD7) continue;
        if (marker == 0x01) continue;

        int len = u16(d);
        if (len < 2) return IMGF_ERR_DECODE;
        len -= 2;

        if (marker == 0xDB) {
            if (!parse_dqt(d, len)) return IMGF_ERR_DECODE;
        } else if (marker == 0xC4) {
            if (!parse_dht(d, len)) return IMGF_ERR_DECODE;
        } else if (marker == 0xC0 || marker == 0xC2) {
            imgf_err_t e = parse_sof(d, len, marker == 0xC2);
            if (e != IMGF_OK) return e;
            have_sof = true;
        } else if (marker == 0xDD) {
            d->restart_interval = u16(d);
        } else if (marker == 0xDA) {
            if (!have_sof) return IMGF_ERR_DECODE;
            imgf_err_t e = parse_sos(d, len);
            if (e != IMGF_OK) return e;
            break;
        } else if ((marker >= 0xC1 && marker <= 0xCF) && marker != 0xC2 &&
                   marker != 0xC4 && marker != 0xC8) {
            return IMGF_ERR_UNSUPPORTED;
        } else {
            if (!skip_bytes(d, len)) return IMGF_ERR_TRUNCATED;
        }
    }
    return setup(d, opts);
}

/* --- entropy decode -------------------------------------------------------- */

static int read_data_byte(jpegd_t *d) {
    if (d->marker_pending) return 0;
    int b = raw_byte(d);
    if (b < 0) { d->marker_pending = 1; d->marker = 0xD9; return 0; }
    if (b != 0xFF) return b;
    int m = raw_byte(d);
    while (m == 0xFF) m = raw_byte(d);
    if (m == 0x00) return 0xFF;
    if (m < 0) { d->marker_pending = 1; d->marker = 0xD9; return 0; }
    d->marker_pending = 1;
    d->marker = m;
    return 0;
}

/* Tops the bit buffer up to at least 25 bits. The common case is four
 * entropy-coded bytes sitting in the reader's window with no 0xFF among them,
 * which goes straight into the buffer: pulling them one at a time through
 * read_data_byte() and imgf_breader_byte() was the single hottest thing in the
 * decoder. */
static void fill_bits(jpegd_t *d, int n) {
    if (d->bitcnt >= n) return;
    imgf_breader_t *br = &d->br;
    while (d->bitcnt <= 24) {
        if (!d->marker_pending && br->pos + 4 <= br->len) {
            const uint8_t *p = br->buf + br->pos;
            const uint32_t w = ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) |
                               ((uint32_t)p[2] << 8) | p[3];
            const uint32_t x = ~w;
            if (!((x - 0x01010101u) & ~x & 0x80808080u)) {
                const int k = (32 - d->bitcnt) >> 3;
                d->bitbuf = k == 4 ? w
                                   : (d->bitbuf << (k * 8)) | (w >> (32 - k * 8));
                d->bitcnt += k * 8;
                br->pos += (size_t)k;
                continue;
            }
        }
        int b = read_data_byte(d);
        d->bitbuf = (d->bitbuf << 8) | (uint32_t)(b & 0xFF);
        d->bitcnt += 8;
    }
}

static int getbits(jpegd_t *d, int n) {
    if (n == 0) return 0;
    fill_bits(d, n);
    d->bitcnt -= n;
    return (d->bitbuf >> d->bitcnt) & ((1u << n) - 1);
}

static int getbit(jpegd_t *d) { return getbits(d, 1); }

static int receive_extend(jpegd_t *d, int s) {
    int v = getbits(d, s);
    if (v < (1 << (s - 1))) v -= (1 << s) - 1;
    return v;
}

static int huffdecode_slow(jpegd_t *d, const jpegd_huff_t *h) {
    int code = 0;
    for (int l = 1; l <= 16; l++) {
        code = (code << 1) | getbit(d);
        if (h->maxcode[l] >= 0 && code <= h->maxcode[l])
            return h->vals[h->valptr[l] + code - h->mincode[l]];
    }
    d->err = 1;
    return 0;
}

static int huffdecode(jpegd_t *d, const jpegd_huff_t *h) {
    fill_bits(d, IMGF_JPEG_FAST_BITS);
    uint16_t e = h->fast[(d->bitbuf >> (d->bitcnt - IMGF_JPEG_FAST_BITS)) & (IMGF_JPEG_FAST_SIZE - 1)];
    if (e) {
        d->bitcnt -= (e >> 8);
        return e & 0xFF;
    }
    return huffdecode_slow(d, h);
}

/* One block into the band, at whatever size the decode was scaled to. dqc
 * holds dequantized coefficients in natural order. */
static void idct_reduce(jpegd_t *d, const int16_t *dqc, uint8_t *dst, int dst_stride,
                        unsigned rowmask, bool ac) {
    if (!ac) {
        const uint8_t v = idct_dc(dqc[0]);
        for (int oy = 0; oy < d->blk; oy++)
            for (int ox = 0; ox < d->blk; ox++) dst[oy * dst_stride + ox] = v;
        return;
    }
    switch (d->blk) {
        case 8: idct8x8_u8(dqc, rowmask, dst, dst_stride); break;
        case 4: idct_nxn(dqc, rowmask, 4, kIdctA4, dst, dst_stride); break;
        case 2: idct_nxn(dqc, rowmask, 2, kIdctA2, dst, dst_stride); break;
        default: dst[0] = idct_dc(dqc[0]); break;
    }
}

static void decode_block(jpegd_t *d, int ci, uint8_t *dst, int dst_stride) {
    jpegd_comp_t *c = &d->comp[ci];
    const uint16_t *q = d->dqt[c->tq];
    int16_t dqc[64] __attribute__((aligned(16)));
    memset(dqc, 0, sizeof dqc);

    int t = huffdecode(d, &d->dc[c->td]);
    int diff = t ? receive_extend(d, t) : 0;
    c->dcpred += diff;
    dqc[0] = sat16(c->dcpred * q[0]);

    unsigned rowmask = 1;
    bool ac = false;
    int k = 1;
    while (k < 64) {
        int rs = huffdecode(d, &d->ac[c->ta]);
        if (d->err) return;
        int r = rs >> 4, s = rs & 0x0F;
        if (s == 0) {
            if (r == 15) { k += 16; continue; }
            break;
        }
        k += r;
        if (k >= 64) break;
        const int nat = kZigzag[k];
        dqc[nat] = sat16(receive_extend(d, s) * q[nat]);
        rowmask |= 1u << (nat >> 3);
        ac = true;
        k++;
    }
    idct_reduce(d, dqc, dst, dst_stride, rowmask, ac);
}

static bool consume_restart(jpegd_t *d) {
    d->bitcnt = 0;
    d->bitbuf = 0;
    if (d->marker_pending) {
        bool ok = d->marker >= 0xD0 && d->marker <= 0xD7;
        d->marker_pending = 0;
        return ok;
    }
    for (;;) {
        int b = raw_byte(d);
        if (b < 0) return false;
        if (b == 0xFF) {
            int m = raw_byte(d);
            while (m == 0xFF) m = raw_byte(d);
            return m >= 0xD0 && m <= 0xD7;
        }
    }
}

/* One band row, upsampled and colour converted. The fixed-point constants are
 * Q8 and the accumulation is single-rounding so that a SIMD version can match
 * it exactly. */
static void emit_row(jpegd_t *d, int py, uint8_t *dst) {
    const uint8_t *yr = d->plane[0] + (size_t)(py >> d->vsh[0]) * d->pstride[0];
    const int w = (int)d->base.width;

    if (d->out_ch == 1) {
        memcpy(dst, yr, (size_t)w);
        return;
    }

    const uint8_t *cbr = d->plane[1] + (size_t)(py >> d->vsh[1]) * d->pstride[1];
    const uint8_t *crr = d->plane[2] + (size_t)(py >> d->vsh[2]) * d->pstride[2];
    const int hsh1 = d->hsh[1], hsh2 = d->hsh[2];
    int x = 0;
#if IMGF_HAVE_PIE
    if (hsh1 == hsh2 && (w >> 4) > 0) {
        const int n = w >> 4;
        imgf_k_prepare();
        if (hsh1) imgf_k_ycc_rgb_h1(dst, yr, cbr, crr, n);
        else      imgf_k_ycc_rgb_h0(dst, yr, cbr, crr, n);
        x = n << 4;
        dst += (size_t)x * 3;
    }
#endif
    for (; x < w; x++) {
        const int y0 = yr[x] << 8;
        const int cb = cbr[x >> hsh1] - 128;
        const int cr = crr[x >> hsh2] - 128;
        *dst++ = clamp8((y0 + 359 * cr) >> 8);
        *dst++ = clamp8((y0 - 88 * cb - 183 * cr) >> 8);
        *dst++ = clamp8((y0 + 454 * cb) >> 8);
    }
}

static void band_row_extent(jpegd_t *d) {
    int base = d->mcu_row_idx * d->blk * d->vmax;
    d->band_valid_rows = d->blk * d->vmax;
    if (base + d->band_valid_rows > (int)d->base.height)
        d->band_valid_rows = (int)d->base.height - base;
}

static void decode_mcu_row(jpegd_t *d) {
    band_row_extent(d);

    for (int mx = 0; mx < d->mcus_per_row && !d->err; mx++) {
        if (d->restart_interval && d->mcu_count > 0 && d->mcu_count % d->restart_interval == 0) {
            if (!consume_restart(d)) { d->err = 1; return; }
            for (int i = 0; i < d->ncomp; i++) d->comp[i].dcpred = 0;
        }
        for (int ci = 0; ci < d->ncomp; ci++) {
            jpegd_comp_t *c = &d->comp[ci];
            const int stride = d->pstride[ci];
            uint8_t *base = d->plane[ci] + (size_t)mx * c->h * d->blk;
            for (int by = 0; by < c->v; by++)
                for (int bx = 0; bx < c->h; bx++)
                    decode_block(d, ci, base + (size_t)(by * d->blk) * stride + bx * d->blk,
                                 stride);
        }
        d->mcu_count++;
    }
    d->band_row = 0;
    d->mcu_row_idx++;
}

/* --- progressive ----------------------------------------------------------- */

static void prog_dc_first(jpegd_t *d, jpegd_comp_t *c, int16_t *blk) {
    int t = huffdecode(d, &d->dc[c->td]);
    if (d->err) return;
    c->dcpred += t ? receive_extend(d, t) : 0;
    blk[0] = (int16_t)(c->dcpred * (1 << d->al));
}

static void prog_dc_refine(jpegd_t *d, int16_t *blk) {
    if (getbit(d)) blk[0] |= (int16_t)(1 << d->al);
}

static void prog_ac_first(jpegd_t *d, jpegd_comp_t *c, int16_t *blk) {
    if (d->eobrun > 0) { d->eobrun--; return; }
    int k = d->ss;
    do {
        int rs = huffdecode(d, &d->ac[c->ta]);
        if (d->err) return;
        int r = rs >> 4, s = rs & 0x0F;
        if (s == 0) {
            if (r < 15) {
                d->eobrun = (1 << r) - 1;
                if (r) d->eobrun += getbits(d, r);
                break;
            }
            k += 16;
        } else {
            k += r;
            if (k > d->se) { d->err = 1; return; }
            blk[kZigzag[k]] = (int16_t)(receive_extend(d, s) * (1 << d->al));
            k++;
        }
    } while (k <= d->se);
}

/* Correction bits for coefficients already sent: every non-zero coefficient in
 * the band takes one bit, and the run lengths only count the ones still zero. */
static void prog_ac_refine(jpegd_t *d, jpegd_comp_t *c, int16_t *blk) {
    const int p1 = 1 << d->al;
    const int m1 = -p1;
    int k = d->ss;

    if (d->eobrun > 0) {
        d->eobrun--;
        for (; k <= d->se; k++) {
            int16_t *p = &blk[kZigzag[k]];
            if (*p && getbit(d) && (*p & p1) == 0) *p += (int16_t)(*p > 0 ? p1 : m1);
        }
        return;
    }

    do {
        int rs = huffdecode(d, &d->ac[c->ta]);
        if (d->err) return;
        int r = rs >> 4, s = rs & 0x0F;
        if (s == 0) {
            if (r < 15) {
                d->eobrun = (1 << r) - 1;
                if (r) d->eobrun += getbits(d, r);
                r = 64;  /* refine the rest of this block, then stop */
            }
        } else {
            if (s != 1) { d->err = 1; return; }
            s = getbit(d) ? p1 : m1;
        }
        while (k <= d->se) {
            int16_t *p = &blk[kZigzag[k++]];
            if (*p) {
                if (getbit(d) && (*p & p1) == 0) *p += (int16_t)(*p > 0 ? p1 : m1);
            } else {
                if (r == 0) {
                    if (s) *p = (int16_t)s;
                    break;
                }
                r--;
            }
        }
    } while (k <= d->se);
}

static void decode_block_prog(jpegd_t *d, jpegd_comp_t *c, int16_t *blk) {
    if (d->ss == 0) {
        if (d->ah == 0) prog_dc_first(d, c, blk);
        else            prog_dc_refine(d, blk);
    } else {
        if (d->ah == 0) prog_ac_first(d, c, blk);
        else            prog_ac_refine(d, c, blk);
    }
}

static bool scan_restart(jpegd_t *d, uint32_t n) {
    if (!d->restart_interval || n == 0 || n % (uint32_t)d->restart_interval) return true;
    if (!consume_restart(d)) { d->err = 1; return false; }
    for (int i = 0; i < d->ncomp; i++) d->comp[i].dcpred = 0;
    d->eobrun = 0;
    return true;
}

static void decode_scan(jpegd_t *d) {
    d->bitbuf = 0;
    d->bitcnt = 0;
    d->eobrun = 0;
    for (int i = 0; i < d->ncomp; i++) d->comp[i].dcpred = 0;

    uint32_t n = 0;
    if (d->scan_ncomp == 1) {
        /* Non-interleaved: the scan walks the component's own block grid, so
           the blocks padding the last MCU are not coded. */
        jpegd_comp_t *c = &d->comp[d->scan_comp[0]];
        for (int by = 0; by < c->nbh && !d->err; by++) {
            for (int bx = 0; bx < c->nbw && !d->err; bx++) {
                if (!scan_restart(d, n)) return;
                decode_block_prog(d, c, c->rows[by] + (size_t)bx * 64);
                n++;
            }
        }
        return;
    }
    for (int my = 0; my < d->mcu_rows && !d->err; my++) {
        for (int mx = 0; mx < d->mcus_per_row && !d->err; mx++) {
            if (!scan_restart(d, n)) return;
            for (int i = 0; i < d->scan_ncomp; i++) {
                jpegd_comp_t *c = &d->comp[d->scan_comp[i]];
                for (int by = 0; by < c->v; by++)
                    for (int bx = 0; bx < c->h; bx++)
                        decode_block_prog(d, c, c->rows[my * c->v + by] +
                                                    (size_t)(mx * c->h + bx) * 64);
            }
            n++;
        }
    }
}

/* Next marker after a scan's entropy data: either the one the bit reader
 * already ran into, or the next one in the stream. Restart markers left over
 * from a short scan are skipped. Returns -1 at end of input. */
static int next_marker(jpegd_t *d) {
    for (;;) {
        int m;
        if (d->marker_pending) {
            m = d->marker;
            d->marker_pending = 0;
        } else {
            int b = raw_byte(d);
            if (b < 0) return -1;
            if (b != 0xFF) continue;
            m = raw_byte(d);
            while (m == 0xFF) m = raw_byte(d);
            if (m < 0) return -1;
            if (m == 0x00) continue;
        }
        if (m >= 0xD0 && m <= 0xD7) continue;
        return m;
    }
}

/* One allocation per block row rather than one per component: a 5 Mpx frame
 * wants 10 MB for its luma coefficients alone, and a heap that has the bytes
 * free in total rarely has them in one piece. */
static imgf_err_t prog_alloc(jpegd_t *d) {
    for (int i = 0; i < d->ncomp; i++) {
        jpegd_comp_t *c = &d->comp[i];
        size_t row_bytes = (size_t)c->bw * 64 * sizeof(int16_t);
        c->rows = (int16_t **)imgf_alloc((size_t)c->bh * sizeof(int16_t *), d->alloc_caps);
        if (!c->rows) return IMGF_ERR_OOM;
        memset(c->rows, 0, (size_t)c->bh * sizeof(int16_t *));
        for (int by = 0; by < c->bh; by++) {
            c->rows[by] = (int16_t *)imgf_alloc(row_bytes, d->alloc_caps);
            if (!c->rows[by]) return IMGF_ERR_OOM;
            memset(c->rows[by], 0, row_bytes);
        }
    }
    return IMGF_OK;
}

/* Reads every scan of the frame into the coefficient arrays. A stream that
 * ends or goes bad part-way keeps what the scans so far refined: a coarse
 * picture beats no picture. */
static imgf_err_t prog_decode(jpegd_t *d) {
    imgf_err_t err = prog_alloc(d);
    if (err != IMGF_OK) return err;

    bool more = true;
    while (more) {
        decode_scan(d);
        if (d->err) break;
        /* Tables may be redefined between scans; keep reading segments until
           the next SOS says which coefficients the next scan carries. */
        for (;;) {
            int marker = next_marker(d);
            if (marker < 0 || marker == 0xD9) { more = false; break; }
            if (marker == 0x01) continue;
            int len = u16(d);
            if (len < 2) { more = false; break; }
            len -= 2;
            if (marker == 0xDA) {
                if (parse_sos(d, len) != IMGF_OK) more = false;
                break;
            }
            if (marker == 0xC4) {
                if (!parse_dht(d, len)) { more = false; break; }
            } else if (marker == 0xDB) {
                if (!parse_dqt(d, len)) { more = false; break; }
            } else if (marker == 0xDD) {
                d->restart_interval = u16(d);
            } else if (!skip_bytes(d, len)) {
                more = false;
                break;
            }
        }
    }
    d->err = 0;
    build_dq_tables(d);
    return IMGF_OK;
}

static void output_mcu_row_prog(jpegd_t *d) {
    band_row_extent(d);

    for (int ci = 0; ci < d->ncomp; ci++) {
        jpegd_comp_t *c = &d->comp[ci];
        const int stride = d->pstride[ci];
        for (int by = 0; by < c->v; by++) {
            const int16_t *row = c->rows[d->mcu_row_idx * c->v + by];
            uint8_t *out = d->plane[ci] + (size_t)(by * d->blk) * stride;
            const uint16_t *q = d->dqt[c->tq];
            for (int bx = 0; bx < c->bw; bx++) {
                const int16_t *src = row + (size_t)bx * 64;
                int16_t dqc[64] __attribute__((aligned(16)));
                unsigned rowmask = 1;
                bool ac = false;
                dqc[0] = sat16(src[0] * q[0]);
                for (int i = 1; i < 64; i++) {
                    if (src[i]) {
                        dqc[i] = sat16(src[i] * q[i]);
                        rowmask |= 1u << (i >> 3);
                        ac = true;
                    } else {
                        dqc[i] = 0;
                    }
                }
                idct_reduce(d, dqc, out + bx * d->blk, stride, rowmask, ac);
            }
        }
    }
    d->band_row = 0;
    d->mcu_row_idx++;
}

/* --- vtable ---------------------------------------------------------------- */

static imgf_err_t jpegd_open(imgf_decoder_t *base, imgf_stream_t src,
                            const imgf_decode_opts_t *opts) {
    jpegd_t *d = (jpegd_t *)base;
    if (!imgf_breader_init(&d->br, src)) return IMGF_ERR_OOM;
    d->br_ready = 1;
    d->alloc_caps = opts->alloc_caps;
    return parse_header(d, opts);
}

static bool jpegd_next_row(imgf_decoder_t *base, uint8_t *dst) {
    jpegd_t *d = (jpegd_t *)base;
    if (d->err) {
        if (base->last_error == IMGF_OK) base->last_error = IMGF_ERR_TRUNCATED;
        return false;
    }
    if (d->progressive && !d->prog_ready) {
        imgf_err_t err = prog_decode(d);
        if (err != IMGF_OK) {
            base->last_error = err;
            d->err = 1;
            return false;
        }
        d->prog_ready = 1;
    }
    if (d->out_row >= base->height) return false;
    if (d->band_row >= d->band_valid_rows) {
        if (d->mcu_row_idx >= d->mcu_rows) return false;
        if (d->progressive) output_mcu_row_prog(d);
        else               decode_mcu_row(d);
        if (d->err) {
            base->last_error = IMGF_ERR_TRUNCATED;
            return false;
        }
    }
    emit_row(d, d->band_row, dst);
    d->band_row++;
    d->out_row++;
    return true;
}

static void jpegd_destroy(imgf_decoder_t *base) {
    jpegd_t *d = (jpegd_t *)base;
    if (d->br_ready) imgf_breader_deinit(&d->br);
    if (d->band) imgf_free(d->band);
    for (int i = 0; i < d->ncomp; i++) {
        jpegd_comp_t *c = &d->comp[i];
        if (!c->rows) continue;
        for (int by = 0; by < c->bh; by++) imgf_free(c->rows[by]);
        imgf_free(c->rows);
    }
    free(d);
}

static const imgf_decoder_vtable_t k_jpegd_vt = {
    jpegd_open, jpegd_next_row, jpegd_destroy,
};

imgf_decoder_t *imgf_jpegd_create(void) {
    jpegd_t *d = (jpegd_t *)calloc(1, sizeof *d);
    if (!d) return NULL;
    d->base.vt     = &k_jpegd_vt;
    d->base.pixfmt = IMGF_PIX_GRAY8;
    d->hmax = d->vmax = 1;
    d->scale = 1;
    d->blk   = 8;
    return &d->base;
}
