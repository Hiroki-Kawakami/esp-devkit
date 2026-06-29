/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * PNG decoder: streaming inflate -> scanline unfilter -> per-row color
 * normalization. Single-pass over the input stream.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_breader.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"
#include "imgf_inflate.h"
#include "imgf_pngd.h"

typedef struct {
    imgf_breader_t *br;
    uint32_t        remaining;
    uint8_t         done;
    uint8_t         buf[2048];
} idat_source_t;

typedef struct {
    imgf_decoder_t base;

    imgf_breader_t br;
    uint8_t        br_ready;

    idat_source_t   idat;
    imgf_inflate_t  inflate;
    uint8_t         inflate_ready;

    uint8_t  bitdepth;
    uint8_t  colortype;
    int      src_channels;
    uint8_t  out_rgb;
    size_t   rowbytes;
    int      filt_bpp;
    uint32_t row;

    uint8_t  palette[256][3];
    uint8_t  trns[256];
    int      palette_count;

    uint8_t *prev;
    uint8_t *cur;
    uint8_t *filtbuf;
} pngd_t;

static uint32_t be32(const uint8_t *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}

static bool read_full(imgf_breader_t *br, uint8_t *buf, size_t n) {
    return imgf_breader_read(br, buf, n) == n;
}

static bool skip_n(imgf_breader_t *br, size_t n) {
    uint8_t tmp[64];
    while (n) {
        size_t k = n < sizeof tmp ? n : sizeof tmp;
        if (imgf_breader_read(br, tmp, k) != k) return false;
        n -= k;
    }
    return true;
}

static bool type_is(const uint8_t *t, char a, char b, char c, char d) {
    return t[0] == (uint8_t)a && t[1] == (uint8_t)b && t[2] == (uint8_t)c && t[3] == (uint8_t)d;
}

static uint8_t composite_white(uint8_t c, uint8_t a) {
    return (uint8_t)((c * a + 255 * (255 - a) + 127) / 255);
}

static const uint8_t *idat_refill(void *user, size_t *n) {
    idat_source_t *s = (idat_source_t *)user;
    while (s->remaining == 0) {
        if (s->done) { *n = 0; return NULL; }
        uint8_t hdr[8];
        if (imgf_breader_read(s->br, hdr, 8) != 8) { s->done = 1; *n = 0; return NULL; }
        uint32_t len = be32(hdr + 4);
        uint8_t type[4];
        if (imgf_breader_read(s->br, type, 4) != 4) { s->done = 1; *n = 0; return NULL; }
        if (type_is(type, 'I', 'D', 'A', 'T')) {
            s->remaining = len;
        } else {
            s->done = 1;
            *n = 0;
            return NULL;
        }
    }
    size_t want = s->remaining < sizeof s->buf ? s->remaining : sizeof s->buf;
    size_t got = imgf_breader_read(s->br, s->buf, want);
    s->remaining -= (uint32_t)got;
    if (got == 0) { s->done = 1; *n = 0; return NULL; }
    *n = got;
    return s->buf;
}

static uint8_t paeth(uint8_t a, uint8_t b, uint8_t c) {
    int p = (int)a + b - c;
    int pa = p > a ? p - a : a - p;
    int pb = p > b ? p - b : b - p;
    int pc = p > c ? p - c : c - p;
    if (pa <= pb && pa <= pc) return a;
    return pb <= pc ? b : c;
}

static void unfilter(pngd_t *d, uint8_t filter, const uint8_t *src, uint8_t *dst) {
    const uint8_t *prev = d->prev;
    int bpp = d->filt_bpp;
    size_t rb = d->rowbytes;
    switch (filter) {
        case 1:
            for (int i = 0; i < bpp; i++) dst[i] = src[i];
            for (size_t i = bpp; i < rb; i++) dst[i] = (uint8_t)(src[i] + dst[i - bpp]);
            break;
        case 2:
            for (size_t i = 0; i < rb; i++) dst[i] = (uint8_t)(src[i] + prev[i]);
            break;
        case 3:
            for (int i = 0; i < bpp; i++) dst[i] = (uint8_t)(src[i] + (prev[i] >> 1));
            for (size_t i = bpp; i < rb; i++)
                dst[i] = (uint8_t)(src[i] + ((dst[i - bpp] + prev[i]) >> 1));
            break;
        case 4:
            for (int i = 0; i < bpp; i++) dst[i] = (uint8_t)(src[i] + prev[i]);
            for (size_t i = bpp; i < rb; i++)
                dst[i] = (uint8_t)(src[i] + paeth(dst[i - bpp], prev[i], prev[i - bpp]));
            break;
        default:
            memcpy(dst, src, rb);
            break;
    }
}

static void convert_row(pngd_t *d, const uint8_t *line, uint8_t *dst) {
    int w = d->base.width;
    switch (d->colortype) {
        case 0: {
            int maxv = (1 << d->bitdepth) - 1;
            for (int x = 0; x < w; x++) {
                int bitpos = x * d->bitdepth;
                int v = (line[bitpos >> 3] >> (8 - d->bitdepth - (bitpos & 7))) & maxv;
                dst[x] = (uint8_t)(v * 255 / maxv);
            }
            break;
        }
        case 2:
            memcpy(dst, line, (size_t)w * 3);
            break;
        case 3: {
            int maxv = (1 << d->bitdepth) - 1;
            for (int x = 0; x < w; x++) {
                int bitpos = x * d->bitdepth;
                int idx = (line[bitpos >> 3] >> (8 - d->bitdepth - (bitpos & 7))) & maxv;
                if (idx >= d->palette_count) idx = d->palette_count - 1;
                uint8_t a = d->trns[idx];
                dst[3 * x + 0] = composite_white(d->palette[idx][0], a);
                dst[3 * x + 1] = composite_white(d->palette[idx][1], a);
                dst[3 * x + 2] = composite_white(d->palette[idx][2], a);
            }
            break;
        }
        case 4:
            for (int x = 0; x < w; x++)
                dst[x] = composite_white(line[2 * x], line[2 * x + 1]);
            break;
        case 6:
            for (int x = 0; x < w; x++) {
                uint8_t a = line[4 * x + 3];
                dst[3 * x + 0] = composite_white(line[4 * x + 0], a);
                dst[3 * x + 1] = composite_white(line[4 * x + 1], a);
                dst[3 * x + 2] = composite_white(line[4 * x + 2], a);
            }
            break;
        default:
            break;
    }
}

/* --- vtable ---------------------------------------------------------------- */

static imgf_err_t pngd_open(imgf_decoder_t *base, imgf_stream_t src,
                           const imgf_decode_opts_t *opts) {
    (void)opts;
    pngd_t *d = (pngd_t *)base;
    if (!imgf_breader_init(&d->br, src)) return IMGF_ERR_OOM;
    d->br_ready = 1;

    static const uint8_t kSig[8] = {0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A};
    uint8_t sig[8];
    if (!read_full(&d->br, sig, 8)) return IMGF_ERR_TRUNCATED;
    if (memcmp(sig, kSig, 8) != 0) return IMGF_ERR_DECODE;

    uint8_t lenbuf[4], type[4];
    if (!read_full(&d->br, lenbuf, 4) || !read_full(&d->br, type, 4)) return IMGF_ERR_TRUNCATED;
    if (!type_is(type, 'I', 'H', 'D', 'R') || be32(lenbuf) != 13) return IMGF_ERR_DECODE;
    uint8_t ih[13];
    if (!read_full(&d->br, ih, 13)) return IMGF_ERR_TRUNCATED;
    if (!skip_n(&d->br, 4)) return IMGF_ERR_TRUNCATED;

    uint32_t w = be32(ih), h = be32(ih + 4);
    d->bitdepth  = ih[8];
    d->colortype = ih[9];
    uint8_t comp = ih[10], filt = ih[11], interlace = ih[12];
    if (comp != 0 || filt != 0) return IMGF_ERR_DECODE;
    if (interlace != 0) return IMGF_ERR_UNSUPPORTED;
    if (w == 0 || h == 0) return IMGF_ERR_DECODE;
    if (w > 0xFFFF || h > 0xFFFF) return IMGF_ERR_TOO_LARGE;

    switch (d->colortype) {
        case 0:
            if (d->bitdepth != 1 && d->bitdepth != 2 && d->bitdepth != 4 && d->bitdepth != 8)
                return IMGF_ERR_UNSUPPORTED;
            d->src_channels = 1; d->out_rgb = 0; break;
        case 2:
            if (d->bitdepth != 8) return IMGF_ERR_UNSUPPORTED;
            d->src_channels = 3; d->out_rgb = 1; break;
        case 3:
            if (d->bitdepth != 1 && d->bitdepth != 2 && d->bitdepth != 4 && d->bitdepth != 8)
                return IMGF_ERR_UNSUPPORTED;
            d->src_channels = 1; d->out_rgb = 1; break;
        case 4:
            if (d->bitdepth != 8) return IMGF_ERR_UNSUPPORTED;
            d->src_channels = 2; d->out_rgb = 0; break;
        case 6:
            if (d->bitdepth != 8) return IMGF_ERR_UNSUPPORTED;
            d->src_channels = 4; d->out_rgb = 1; break;
        default:
            return IMGF_ERR_UNSUPPORTED;
    }

    memset(d->trns, 0xFF, sizeof d->trns);

    for (;;) {
        if (!read_full(&d->br, lenbuf, 4) || !read_full(&d->br, type, 4)) return IMGF_ERR_TRUNCATED;
        uint32_t len = be32(lenbuf);
        if (type_is(type, 'P', 'L', 'T', 'E')) {
            if (len % 3 != 0 || len / 3 > 256) return IMGF_ERR_DECODE;
            d->palette_count = (int)(len / 3);
            for (int i = 0; i < d->palette_count; i++)
                if (!read_full(&d->br, d->palette[i], 3)) return IMGF_ERR_TRUNCATED;
            if (!skip_n(&d->br, 4)) return IMGF_ERR_TRUNCATED;
        } else if (type_is(type, 't', 'R', 'N', 'S')) {
            uint32_t k = len > 256 ? 256 : len;
            if (k && imgf_breader_read(&d->br, d->trns, k) != k) return IMGF_ERR_TRUNCATED;
            if (!skip_n(&d->br, len - k + 4)) return IMGF_ERR_TRUNCATED;
        } else if (type_is(type, 'I', 'D', 'A', 'T')) {
            d->idat.br = &d->br;
            d->idat.remaining = len;
            break;
        } else if (type_is(type, 'I', 'E', 'N', 'D')) {
            return IMGF_ERR_DECODE;
        } else {
            if (!skip_n(&d->br, len + 4)) return IMGF_ERR_TRUNCATED;
        }
    }

    if (d->colortype == 3 && d->palette_count == 0) return IMGF_ERR_DECODE;

    imgf_inflate_src_t isrc = { idat_refill, &d->idat };
    if (!imgf_inflate_init(&d->inflate, isrc)) return IMGF_ERR_DECODE;
    d->inflate_ready = 1;

    d->base.width  = (uint16_t)w;
    d->base.height = (uint16_t)h;
    d->base.pixfmt = d->out_rgb ? IMGF_PIX_RGB888 : IMGF_PIX_GRAY8;

    d->rowbytes = ((size_t)w * d->src_channels * d->bitdepth + 7) / 8;
    d->filt_bpp = (d->src_channels * d->bitdepth + 7) / 8;
    d->row = 0;

    d->prev    = (uint8_t *)calloc(d->rowbytes, 1);
    d->cur     = (uint8_t *)malloc(d->rowbytes);
    d->filtbuf = (uint8_t *)malloc(1 + d->rowbytes);
    if (!d->prev || !d->cur || !d->filtbuf) return IMGF_ERR_OOM;

    return IMGF_OK;
}

static bool pngd_next_row(imgf_decoder_t *base, uint8_t *dst) {
    pngd_t *d = (pngd_t *)base;
    if (d->row >= base->height) return false;
    size_t need = 1 + d->rowbytes;

    size_t got = imgf_inflate_read(&d->inflate, d->filtbuf, need);
    if (got != need || imgf_inflate_failed(&d->inflate)) {
        base->last_error = IMGF_ERR_TRUNCATED;
        return false;
    }

    unfilter(d, d->filtbuf[0], d->filtbuf + 1, d->cur);
    convert_row(d, d->cur, dst);

    uint8_t *tmp = d->prev;
    d->prev = d->cur;
    d->cur  = tmp;
    d->row++;
    return true;
}

static void pngd_destroy(imgf_decoder_t *base) {
    pngd_t *d = (pngd_t *)base;
    if (d->inflate_ready) imgf_inflate_deinit(&d->inflate);
    if (d->br_ready) imgf_breader_deinit(&d->br);
    free(d->prev);
    free(d->cur);
    free(d->filtbuf);
    free(d);
}

static const imgf_decoder_vtable_t k_pngd_vt = {
    pngd_open, pngd_next_row, pngd_destroy,
};

imgf_decoder_t *imgf_pngd_create(void) {
    pngd_t *d = (pngd_t *)calloc(1, sizeof *d);
    if (!d) return NULL;
    d->base.vt     = &k_pngd_vt;
    d->base.pixfmt = IMGF_PIX_GRAY8;
    return &d->base;
}
