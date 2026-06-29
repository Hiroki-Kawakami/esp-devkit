/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Baseline JPEG decoder. Pulls bytes from imgf_breader_t and serves rows
 * top-to-bottom; decodes one MCU row at a time into a band buffer with an
 * AAN-prescaled fast IDCT and per-block sub-sampled IDCT (1/N for N in 2..8).
 */

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_breader.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"
#include "imgf_jpegd.h"

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
    int      aan_qt[4][64];
    jpegd_huff_t dc[4];
    jpegd_huff_t ac[4];
    int      restart_interval;

    int      scale;
    int      blk;
    int      mcus_per_row;
    int      mcu_rows;
    int      mcu_row_idx;

    uint8_t *band;
    int      band_w;
    int      band_h;
    int      band_valid_rows;
    int      band_row;
    uint32_t out_row;

    uint32_t bitbuf;
    int      bitcnt;
    uint8_t  marker_pending;
    int      marker;
    uint32_t mcu_count;

    uint8_t  compbuf[3][16 * 16];
    uint8_t  err;
} jpegd_t;

static const uint8_t kZigzag[64] = {
     0,  1,  8, 16,  9,  2,  3, 10, 17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63};

/* AAN per-frequency scale factors; folded into the dequant table so the fast
 * IDCT (jidctfst-style) needs no per-output scaling. */
static const double kAanScale[8] = {
    1.0, 1.387039845, 1.306562965, 1.175875602,
    1.0, 0.785694958, 0.541196100, 0.275899379};

static const int kFix1_082 = 277;
static const int kFix1_414 = 362;
static const int kFix1_847 = 473;
static const int kFix2_613 = 669;

static inline uint8_t clamp8(int v) {
    return v < 0 ? 0 : (v > 255 ? 255 : (uint8_t)v);
}

static inline int imul(int v, int c) {
    return (int)(((int64_t)v * c) >> 8);
}

static inline int idescale(int x, int n) {
    return (x + (1 << (n - 1))) >> n;
}

/* jidctfst (Arai-Agui-Nakajima). coeff: raw (un-dequantized) coefficients in
 * natural order; q: AAN-prescaled dequant table. Writes the level-shift-free
 * spatial block to `out`. */
static void idct8x8_fast(const int *coeff, const int *q, int *out) {
    int ws[64];
    for (int c = 0; c < 8; c++) {
        const int *in = coeff + c;
        const int *qq = q + c;
        if (!in[8] && !in[16] && !in[24] && !in[32] && !in[40] && !in[48] && !in[56]) {
            int dc = in[0] * qq[0];
            for (int r = 0; r < 8; r++) ws[c + r * 8] = dc;
            continue;
        }
        int tmp0 = in[0]  * qq[0];
        int tmp1 = in[16] * qq[16];
        int tmp2 = in[32] * qq[32];
        int tmp3 = in[48] * qq[48];
        int tmp10 = tmp0 + tmp2, tmp11 = tmp0 - tmp2;
        int tmp13 = tmp1 + tmp3;
        int tmp12 = imul(tmp1 - tmp3, kFix1_414) - tmp13;
        tmp0 = tmp10 + tmp13;
        tmp3 = tmp10 - tmp13;
        tmp1 = tmp11 + tmp12;
        tmp2 = tmp11 - tmp12;
        int tmp4 = in[8]  * qq[8];
        int tmp5 = in[24] * qq[24];
        int tmp6 = in[40] * qq[40];
        int tmp7 = in[56] * qq[56];
        int z13 = tmp6 + tmp5, z10 = tmp6 - tmp5;
        int z11 = tmp4 + tmp7, z12 = tmp4 - tmp7;
        tmp7 = z11 + z13;
        tmp11 = imul(z11 - z13, kFix1_414);
        int z5 = imul(z10 + z12, kFix1_847);
        tmp10 = imul(z12, kFix1_082) - z5;
        tmp12 = imul(z10, -kFix2_613) + z5;
        tmp6 = tmp12 - tmp7;
        tmp5 = tmp11 - tmp6;
        tmp4 = tmp10 + tmp5;
        ws[c + 0 * 8] = tmp0 + tmp7;
        ws[c + 7 * 8] = tmp0 - tmp7;
        ws[c + 1 * 8] = tmp1 + tmp6;
        ws[c + 6 * 8] = tmp1 - tmp6;
        ws[c + 2 * 8] = tmp2 + tmp5;
        ws[c + 5 * 8] = tmp2 - tmp5;
        ws[c + 4 * 8] = tmp3 + tmp4;
        ws[c + 3 * 8] = tmp3 - tmp4;
    }
    for (int r = 0; r < 8; r++) {
        const int *w = ws + r * 8;
        int *o = out + r * 8;
        int tmp10 = w[0] + w[4], tmp11 = w[0] - w[4];
        int tmp13 = w[2] + w[6];
        int tmp12 = imul(w[2] - w[6], kFix1_414) - tmp13;
        int tmp0 = tmp10 + tmp13, tmp3 = tmp10 - tmp13;
        int tmp1 = tmp11 + tmp12, tmp2 = tmp11 - tmp12;
        int z13 = w[5] + w[3], z10 = w[5] - w[3];
        int z11 = w[1] + w[7], z12 = w[1] - w[7];
        int tmp7 = z11 + z13;
        int tmp11b = imul(z11 - z13, kFix1_414);
        int z5 = imul(z10 + z12, kFix1_847);
        int tmp10b = imul(z12, kFix1_082) - z5;
        int tmp12b = imul(z10, -kFix2_613) + z5;
        int tmp6 = tmp12b - tmp7;
        int tmp5 = tmp11b - tmp6;
        int tmp4 = tmp10b + tmp5;
        o[0] = idescale(tmp0 + tmp7, 5);
        o[7] = idescale(tmp0 - tmp7, 5);
        o[1] = idescale(tmp1 + tmp6, 5);
        o[6] = idescale(tmp1 - tmp6, 5);
        o[2] = idescale(tmp2 + tmp5, 5);
        o[5] = idescale(tmp2 - tmp5, 5);
        o[4] = idescale(tmp3 + tmp4, 5);
        o[3] = idescale(tmp3 - tmp4, 5);
    }
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

static imgf_err_t parse_sof(jpegd_t *d, int len) {
    (void)len;
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
        if (id < 0 || hv < 0 || tq < 0) return IMGF_ERR_DECODE;
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
    if (ns != d->ncomp) return IMGF_ERR_UNSUPPORTED;
    for (int i = 0; i < ns; i++) {
        int cs = raw_byte(d), td_ta = raw_byte(d);
        if (cs < 0 || td_ta < 0) return IMGF_ERR_DECODE;
        int ci = -1;
        for (int j = 0; j < d->ncomp; j++)
            if (d->comp[j].id == cs) ci = j;
        if (ci < 0) return IMGF_ERR_DECODE;
        d->comp[ci].td = td_ta >> 4;
        d->comp[ci].ta = td_ta & 0x0F;
    }
    raw_byte(d);
    raw_byte(d);
    raw_byte(d);
    return IMGF_OK;
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

    d->base.width  = (uint16_t)((d->w + d->scale - 1) / d->scale);
    d->base.height = (uint16_t)((d->h + d->scale - 1) / d->scale);
    d->out_ch      = d->ncomp == 1 ? 1 : 3;
    d->base.pixfmt = d->ncomp == 1 ? IMGF_PIX_GRAY8 : IMGF_PIX_RGB888;

    d->band_w = d->mcus_per_row * d->blk * d->hmax;
    d->band_h = d->blk * d->vmax;
    size_t band_bytes = (size_t)d->band_w * d->band_h * d->out_ch;
    d->band = band_bytes <= 96 * 1024 ? (uint8_t *)imgf_alloc_internal(band_bytes) : NULL;
    if (!d->band) d->band = (uint8_t *)imgf_alloc(band_bytes, d->alloc_caps);
    if (!d->band) return IMGF_ERR_OOM;

    for (int i = 0; i < d->ncomp; i++) d->comp[i].dcpred = 0;

    for (int i = 0; i < d->ncomp; i++) {
        int tq = d->comp[i].tq;
        for (int k = 0; k < 64; k++) {
            int nat = kZigzag[k];
            d->aan_qt[tq][nat] = (int)lround(
                d->qt[tq][k] * kAanScale[nat >> 3] * kAanScale[nat & 7] * 4.0);
        }
    }
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
        } else if (marker == 0xC0) {
            imgf_err_t e = parse_sof(d, len);
            if (e != IMGF_OK) return e;
            have_sof = true;
        } else if (marker == 0xDD) {
            d->restart_interval = u16(d);
        } else if (marker == 0xDA) {
            if (!have_sof) return IMGF_ERR_DECODE;
            imgf_err_t e = parse_sos(d, len);
            if (e != IMGF_OK) return e;
            break;
        } else if ((marker >= 0xC1 && marker <= 0xCF) && marker != 0xC4 && marker != 0xC8) {
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

static void fill_bits(jpegd_t *d, int n) {
    while (d->bitcnt < n) {
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
    if (v < (1 << (s - 1))) v += (-1 << s) + 1;
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

static void idct_reduce(jpegd_t *d, const int *coeff, const int *q,
                        uint8_t *dst, int dst_stride, bool ac) {
    if (d->blk == 1) {
        dst[0] = clamp8(((coeff[0] * q[0] + 16) >> 5) + 128);
        return;
    }
    if (!ac) {
        int v = clamp8(((coeff[0] * q[0] + 16) >> 5) + 128);
        for (int oy = 0; oy < d->blk; oy++)
            for (int ox = 0; ox < d->blk; ox++) dst[oy * dst_stride + ox] = (uint8_t)v;
        return;
    }

    int spatial[64];
    idct8x8_fast(coeff, q, spatial);
    if (d->blk == 8) {
        for (int y = 0; y < 8; y++)
            for (int x = 0; x < 8; x++)
                dst[y * dst_stride + x] = clamp8(spatial[y * 8 + x] + 128);
        return;
    }
    int f = 8 / d->blk, area = f * f;
    for (int oy = 0; oy < d->blk; oy++) {
        for (int ox = 0; ox < d->blk; ox++) {
            int sum = 0;
            for (int yy = 0; yy < f; yy++)
                for (int xx = 0; xx < f; xx++)
                    sum += spatial[(oy * f + yy) * 8 + ox * f + xx];
            dst[oy * dst_stride + ox] = clamp8(sum / area + 128);
        }
    }
}

static void decode_block(jpegd_t *d, int ci, uint8_t *dst, int dst_stride) {
    jpegd_comp_t *c = &d->comp[ci];
    int coeff[64];
    memset(coeff, 0, sizeof coeff);

    int t = huffdecode(d, &d->dc[c->td]);
    int diff = t ? receive_extend(d, t) : 0;
    c->dcpred += diff;
    coeff[0] = c->dcpred;

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
        coeff[kZigzag[k]] = receive_extend(d, s);
        ac = true;
        k++;
    }
    idct_reduce(d, coeff, d->aan_qt[c->tq], dst, dst_stride, ac);
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

static void decode_mcu_row(jpegd_t *d) {
    int base = d->mcu_row_idx * d->blk * d->vmax;
    d->band_valid_rows = d->blk * d->vmax;
    if (base + d->band_valid_rows > (int)d->base.height)
        d->band_valid_rows = (int)d->base.height - base;

    int vsh[3], hsh[3], cstride[3];
    for (int i = 0; i < d->ncomp; i++) {
        vsh[i]     = d->comp[i].v == d->vmax ? 0 : 1;
        hsh[i]     = d->comp[i].h == d->hmax ? 0 : 1;
        cstride[i] = d->comp[i].h * d->blk;
    }
    int mcu_w = d->blk * d->hmax;

    for (int mx = 0; mx < d->mcus_per_row && !d->err; mx++) {
        if (d->restart_interval && d->mcu_count > 0 && d->mcu_count % d->restart_interval == 0) {
            if (!consume_restart(d)) { d->err = 1; return; }
            for (int i = 0; i < d->ncomp; i++) d->comp[i].dcpred = 0;
        }
        for (int ci = 0; ci < d->ncomp; ci++) {
            jpegd_comp_t *c = &d->comp[ci];
            int cb_w = c->h * d->blk;
            for (int by = 0; by < c->v; by++)
                for (int bx = 0; bx < c->h; bx++)
                    decode_block(d, ci, d->compbuf[ci] + (by * d->blk) * cb_w + bx * d->blk, cb_w);
        }

        int px0 = mx * mcu_w;
        for (int py = 0; py < d->band_h; py++) {
            uint8_t *out = d->band + ((size_t)py * d->band_w + px0) * d->out_ch;
            const uint8_t *yr = d->compbuf[0] + (py >> vsh[0]) * cstride[0];
            if (d->out_ch == 1) {
                for (int px = 0; px < mcu_w; px++) *out++ = yr[px >> hsh[0]];
            } else {
                const uint8_t *cbr = d->compbuf[1] + (py >> vsh[1]) * cstride[1];
                const uint8_t *crr = d->compbuf[2] + (py >> vsh[2]) * cstride[2];
                for (int px = 0; px < mcu_w; px++) {
                    int y0 = yr[px >> hsh[0]];
                    int cb = cbr[px >> hsh[1]] - 128;
                    int cr = crr[px >> hsh[2]] - 128;
                    *out++ = clamp8(y0 + ((91881 * cr) >> 16));
                    *out++ = clamp8(y0 - ((22554 * cb + 46802 * cr) >> 16));
                    *out++ = clamp8(y0 + ((116130 * cb) >> 16));
                }
            }
        }
        d->mcu_count++;
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
    if (d->out_row >= base->height) return false;
    if (d->band_row >= d->band_valid_rows) {
        if (d->mcu_row_idx >= d->mcu_rows) return false;
        decode_mcu_row(d);
        if (d->err) {
            base->last_error = IMGF_ERR_TRUNCATED;
            return false;
        }
    }
    memcpy(dst, d->band + (size_t)d->band_row * d->band_w * d->out_ch,
           (size_t)base->width * d->out_ch);
    d->band_row++;
    d->out_row++;
    return true;
}

static void jpegd_destroy(imgf_decoder_t *base) {
    jpegd_t *d = (jpegd_t *)base;
    if (d->br_ready) imgf_breader_deinit(&d->br);
    if (d->band) imgf_free(d->band);
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
