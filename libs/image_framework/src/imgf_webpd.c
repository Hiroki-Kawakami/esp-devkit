/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * WebP container: RIFF chunks, the ALPH plane and the first animation frame.
 * The codecs are imgf_webpd_vp8.c and imgf_webpd_vp8l.c.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"
#include "imgf_webpd.h"
#include "imgf_webpd_internal.h"

#define ALPHA_WINDOW 8192

typedef struct {
    imgf_decoder_t base;

    webp_source_t src;
    uint32_t      alloc_caps;

    webp_vp8_t  *vp8;
    webp_vp8l_t *vp8l;
    uint32_t    *argb;
    bool         has_alpha;

    uint8_t        alpha_filter;
    webp_cursor_t  alpha_raw;
    const uint8_t *alpha_p;
    const uint8_t *alpha_e;
    webp_vp8l_t   *alpha_vp8l;
    uint32_t      *alpha_argb;
    uint8_t       *alpha_in;
    uint8_t       *alpha;

    uint32_t frame_x;
    uint32_t frame_y;
    uint32_t frame_w;
    uint32_t frame_h;
    uint32_t row;
} webpd_t;

typedef struct {
    size_t pos;
    size_t len;
} chunk_t;

static uint32_t le24(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16);
}

static uint32_t le32(const uint8_t *p) {
    return le24(p) | ((uint32_t)p[3] << 24);
}

static uint8_t over_white(uint8_t c, uint8_t a) {
    return (uint8_t)((c * a + 255 * (255 - a) + 127) / 255);
}

size_t webp_source_read(webp_source_t *src, size_t pos, void *dst, size_t n) {
    if (src->at != pos) {
        if (src->stream.seek) {
            if (src->stream.seek(src->stream.user, pos) < 0) {
                src->truncated = true;
                return 0;
            }
            src->at = pos;
        } else {
            uint8_t tmp[256];
            while (src->at < pos) {
                size_t k = pos - src->at < sizeof tmp ? pos - src->at : sizeof tmp;
                int got = src->stream.read(src->stream.user, tmp, k);
                if (got <= 0) {
                    src->truncated = true;
                    return 0;
                }
                src->at += (size_t)got;
            }
            if (src->at != pos) {
                src->truncated = true;
                return 0;
            }
        }
    }
    uint8_t *d = (uint8_t *)dst;
    size_t total = 0;
    while (total < n) {
        int got = src->stream.read(src->stream.user, d + total, n - total);
        if (got <= 0) break;
        total += (size_t)got;
    }
    src->at += total;
    if (total < n) src->truncated = true;
    return total;
}

bool webp_cursor_init(webp_cursor_t *c, webp_source_t *src, size_t start, size_t len,
                      size_t cap, uint32_t caps) {
    c->src = src;
    c->next = start;
    c->end = start + len;
    if (cap > len) cap = len ? len : 1;
    c->cap = cap;
    c->buf = (uint8_t *)imgf_alloc(cap, caps);
    return c->buf != NULL;
}

void webp_cursor_deinit(webp_cursor_t *c) {
    imgf_free(c->buf);
    c->buf = NULL;
}

const uint8_t *webp_cursor_refill(webp_cursor_t *c, size_t *n) {
    size_t want = c->next < c->end ? c->end - c->next : 0;
    if (want > c->cap) want = c->cap;
    if (want == 0) {
        *n = 0;
        return NULL;
    }
    size_t got = webp_source_read(c->src, c->next, c->buf, want);
    c->next += got;
    if (got < want) c->end = c->next;
    *n = got;
    return got ? c->buf : NULL;
}

static void unfilter(int filter, const uint8_t *prev, const uint8_t *in, uint8_t *out, int w) {
    if (filter == 0) {
        memcpy(out, in, (size_t)w);
        return;
    }
    if (filter == 1 || !prev) {
        uint8_t pred = prev ? prev[0] : 0;
        for (int i = 0; i < w; i++) pred = out[i] = (uint8_t)(pred + in[i]);
        return;
    }
    if (filter == 2) {
        for (int i = 0; i < w; i++) out[i] = (uint8_t)(prev[i] + in[i]);
        return;
    }
    uint8_t top_left = prev[0];
    uint8_t left = prev[0];
    for (int i = 0; i < w; i++) {
        const uint8_t top = prev[i];
        int g = left + top - top_left;
        g = g < 0 ? 0 : g > 255 ? 255 : g;
        left = (uint8_t)(in[i] + g);
        top_left = top;
        out[i] = left;
    }
}

static imgf_err_t alpha_raw_row(webpd_t *d, uint8_t *dst, size_t n) {
    while (n) {
        if (d->alpha_p == d->alpha_e) {
            size_t got;
            d->alpha_p = webp_cursor_refill(&d->alpha_raw, &got);
            if (!got) return webp_eof_error(&d->src);
            d->alpha_e = d->alpha_p + got;
        }
        size_t k = (size_t)(d->alpha_e - d->alpha_p);
        if (k > n) k = n;
        memcpy(dst, d->alpha_p, k);
        d->alpha_p += k;
        dst += k;
        n -= k;
    }
    return IMGF_OK;
}

static imgf_err_t alpha_next_row(webpd_t *d, uint32_t y) {
    const int w = (int)d->frame_w;
    if (d->alpha_vp8l) {
        imgf_err_t err = webp_vp8l_next_row(d->alpha_vp8l, d->alpha_argb);
        if (err != IMGF_OK) return err;
        for (int x = 0; x < w; x++) d->alpha_in[x] = (uint8_t)(d->alpha_argb[x] >> 8);
    } else {
        imgf_err_t err = alpha_raw_row(d, d->alpha_in, (size_t)w);
        if (err != IMGF_OK) return err;
    }
    unfilter(d->alpha_filter, y ? d->alpha : NULL, d->alpha_in, d->alpha, w);
    return IMGF_OK;
}

static imgf_err_t open_alpha(webpd_t *d, chunk_t alph) {
    uint8_t hdr;
    if (alph.len < 2) return IMGF_ERR_DECODE;
    if (webp_source_read(&d->src, alph.pos, &hdr, 1) != 1) return IMGF_ERR_TRUNCATED;
    const int method = hdr & 3;
    const int pre = (hdr >> 4) & 3;
    d->alpha_filter = (hdr >> 2) & 3;
    if (method > 1 || pre > 1 || (hdr >> 6)) return IMGF_ERR_DECODE;

    const size_t w = d->frame_w;
    d->alpha_in = (uint8_t *)imgf_alloc(w, d->alloc_caps);
    d->alpha = (uint8_t *)imgf_alloc(w, d->alloc_caps);
    if (!d->alpha_in || !d->alpha) return IMGF_ERR_OOM;
    if (method == 0) {
        if (alph.len - 1 < w * d->frame_h) return IMGF_ERR_DECODE;
        if (!webp_cursor_init(&d->alpha_raw, &d->src, alph.pos + 1, alph.len - 1, ALPHA_WINDOW,
                              d->alloc_caps)) {
            return IMGF_ERR_OOM;
        }
        return IMGF_OK;
    }
    d->alpha_argb = (uint32_t *)imgf_alloc(w * 4, d->alloc_caps);
    if (!d->alpha_argb) return IMGF_ERR_OOM;
    return webp_vp8l_open(&d->alpha_vp8l, &d->src, alph.pos + 1, alph.len - 1, (int)w,
                          (int)d->frame_h, d->alloc_caps);
}

static imgf_err_t read_chunk(webp_source_t *src, size_t pos, size_t end, uint8_t tag[4],
                             chunk_t *body) {
    uint8_t hdr[8];
    if (pos + 8 > end) return IMGF_ERR_DECODE;
    if (webp_source_read(src, pos, hdr, 8) != 8) return IMGF_ERR_TRUNCATED;
    memcpy(tag, hdr, 4);
    body->pos = pos + 8;
    body->len = le32(hdr + 4);
    if (body->len > end - body->pos) return IMGF_ERR_DECODE;
    return IMGF_OK;
}

static size_t chunk_end(chunk_t c) {
    return c.pos + c.len + (c.len & 1);
}

static imgf_err_t find_image(webp_source_t *src, size_t pos, size_t end, chunk_t *alph,
                             chunk_t *image, bool *lossless) {
    for (;;) {
        uint8_t tag[4];
        chunk_t c;
        imgf_err_t err = read_chunk(src, pos, end, tag, &c);
        if (err != IMGF_OK) return err;
        if (memcmp(tag, "VP8 ", 4) == 0 || memcmp(tag, "VP8L", 4) == 0) {
            *image = c;
            *lossless = tag[3] == 'L';
            return IMGF_OK;
        }
        if (memcmp(tag, "ALPH", 4) == 0) *alph = c;
        pos = chunk_end(c);
    }
}

static imgf_err_t parse(webpd_t *d, chunk_t *alph, chunk_t *image, bool *lossless,
                        uint32_t *canvas_w, uint32_t *canvas_h) {
    uint8_t riff[12];
    if (webp_source_read(&d->src, 0, riff, 12) != 12) return IMGF_ERR_TRUNCATED;
    if (memcmp(riff, "RIFF", 4) != 0 || memcmp(riff + 8, "WEBP", 4) != 0) return IMGF_ERR_DECODE;
    const size_t end = 8 + (size_t)le32(riff + 4);

    uint8_t tag[4];
    chunk_t first;
    imgf_err_t err = read_chunk(&d->src, 12, end, tag, &first);
    if (err != IMGF_OK) return err;
    if (memcmp(tag, "VP8X", 4) != 0) {
        *canvas_w = 0;
        *canvas_h = 0;
        *image = first;
        *lossless = tag[3] == 'L';
        return memcmp(tag, "VP8 ", 4) == 0 || memcmp(tag, "VP8L", 4) == 0 ? IMGF_OK
                                                                         : IMGF_ERR_DECODE;
    }

    uint8_t vp8x[10];
    if (first.len < 10) return IMGF_ERR_DECODE;
    if (webp_source_read(&d->src, first.pos, vp8x, 10) != 10) return IMGF_ERR_TRUNCATED;
    *canvas_w = le24(vp8x + 4) + 1;
    *canvas_h = le24(vp8x + 7) + 1;
    if (!(vp8x[0] & 0x02)) return find_image(&d->src, chunk_end(first), end, alph, image, lossless);

    chunk_t frame;
    for (size_t pos = chunk_end(first);; pos = chunk_end(frame)) {
        err = read_chunk(&d->src, pos, end, tag, &frame);
        if (err != IMGF_OK) return err;
        if (memcmp(tag, "ANMF", 4) == 0) break;
    }
    uint8_t anmf[16];
    if (frame.len < 16) return IMGF_ERR_DECODE;
    if (webp_source_read(&d->src, frame.pos, anmf, 16) != 16) return IMGF_ERR_TRUNCATED;
    d->frame_x = le24(anmf) * 2;
    d->frame_y = le24(anmf + 3) * 2;
    d->frame_w = le24(anmf + 6) + 1;
    d->frame_h = le24(anmf + 9) + 1;
    return find_image(&d->src, frame.pos + 16, frame.pos + frame.len, alph, image, lossless);
}

/* --- vtable ---------------------------------------------------------------- */

static imgf_err_t webpd_open(imgf_decoder_t *base, imgf_stream_t src,
                             const imgf_decode_opts_t *opts) {
    webpd_t *d = (webpd_t *)base;
    d->alloc_caps = opts->alloc_caps;
    d->src.stream = src;

    chunk_t alph = {0, 0};
    chunk_t image;
    bool lossless;
    uint32_t canvas_w, canvas_h;
    imgf_err_t err = parse(d, &alph, &image, &lossless, &canvas_w, &canvas_h);
    if (err != IMGF_OK) return err;
    if (lossless) alph.len = 0;
    if ((!lossless || alph.len) && !src.seek) return IMGF_ERR_UNSUPPORTED;

    int w, h;
    if (lossless) {
        err = webp_vp8l_open(&d->vp8l, &d->src, image.pos, image.len, 0, 0, d->alloc_caps);
        if (err != IMGF_OK) return err;
        w = webp_vp8l_width(d->vp8l);
        h = webp_vp8l_height(d->vp8l);
        d->has_alpha = webp_vp8l_has_alpha(d->vp8l);
        d->argb = (uint32_t *)imgf_alloc((size_t)w * 4, d->alloc_caps);
        if (!d->argb) return IMGF_ERR_OOM;
    } else {
        err = webp_vp8_open(&d->vp8, &d->src, image.pos, image.len, d->alloc_caps);
        if (err != IMGF_OK) return err;
        w = webp_vp8_width(d->vp8);
        h = webp_vp8_height(d->vp8);
        d->has_alpha = alph.len != 0;
    }

    if (d->frame_w) {
        if (d->frame_w != (uint32_t)w || d->frame_h != (uint32_t)h) return IMGF_ERR_DECODE;
        if (d->frame_x + d->frame_w > canvas_w || d->frame_y + d->frame_h > canvas_h) {
            return IMGF_ERR_DECODE;
        }
    } else {
        if (canvas_w && (canvas_w != (uint32_t)w || canvas_h != (uint32_t)h)) {
            return IMGF_ERR_DECODE;
        }
        canvas_w = d->frame_w = (uint32_t)w;
        canvas_h = d->frame_h = (uint32_t)h;
    }
    if (canvas_w > 0xFFFF || canvas_h > 0xFFFF) return IMGF_ERR_TOO_LARGE;

    if (d->has_alpha && !lossless) {
        err = open_alpha(d, alph);
        if (err != IMGF_OK) return err;
    }

    d->base.width = (uint16_t)canvas_w;
    d->base.height = (uint16_t)canvas_h;
    d->base.pixfmt = IMGF_PIX_RGB888;
    d->row = 0;
    return IMGF_OK;
}

static imgf_err_t frame_row(webpd_t *d, uint8_t *out, uint32_t y) {
    const uint32_t w = d->frame_w;
    if (d->vp8l) {
        imgf_err_t err = webp_vp8l_next_row(d->vp8l, d->argb);
        if (err != IMGF_OK) return err;
        for (uint32_t x = 0; x < w; x++, out += 3) {
            const uint32_t p = d->argb[x];
            const uint8_t r = (uint8_t)(p >> 16), g = (uint8_t)(p >> 8), b = (uint8_t)p;
            if (d->has_alpha) {
                const uint8_t a = (uint8_t)(p >> 24);
                out[0] = over_white(r, a);
                out[1] = over_white(g, a);
                out[2] = over_white(b, a);
            } else {
                out[0] = r;
                out[1] = g;
                out[2] = b;
            }
        }
        return IMGF_OK;
    }
    imgf_err_t err = webp_vp8_next_row(d->vp8, out);
    if (err != IMGF_OK || !d->has_alpha) return err;
    err = alpha_next_row(d, y);
    if (err != IMGF_OK) return err;
    for (uint32_t x = 0; x < w; x++, out += 3) {
        const uint8_t a = d->alpha[x];
        out[0] = over_white(out[0], a);
        out[1] = over_white(out[1], a);
        out[2] = over_white(out[2], a);
    }
    return IMGF_OK;
}

static bool webpd_next_row(imgf_decoder_t *base, uint8_t *dst) {
    webpd_t *d = (webpd_t *)base;
    if (d->row >= base->height) return false;
    const uint32_t y = d->row;
    const bool inside = y >= d->frame_y && y < d->frame_y + d->frame_h;
    if (!inside || d->frame_w != base->width) memset(dst, 0xFF, (size_t)base->width * 3);
    if (inside) {
        imgf_err_t err = frame_row(d, dst + (size_t)d->frame_x * 3, y - d->frame_y);
        if (err != IMGF_OK) {
            base->last_error = err;
            return false;
        }
    }
    d->row++;
    return true;
}

static void webpd_destroy(imgf_decoder_t *base) {
    webpd_t *d = (webpd_t *)base;
    webp_vp8_destroy(d->vp8);
    webp_vp8l_destroy(d->vp8l);
    webp_vp8l_destroy(d->alpha_vp8l);
    webp_cursor_deinit(&d->alpha_raw);
    imgf_free(d->argb);
    imgf_free(d->alpha_argb);
    imgf_free(d->alpha_in);
    imgf_free(d->alpha);
    free(d);
}

static const imgf_decoder_vtable_t k_webpd_vt = {
    webpd_open, webpd_next_row, webpd_destroy,
};

imgf_decoder_t *imgf_webpd_create(void) {
    webpd_t *d = (webpd_t *)calloc(1, sizeof *d);
    if (!d) return NULL;
    d->base.vt     = &k_webpd_vt;
    d->base.pixfmt = IMGF_PIX_RGB888;
    return &d->base;
}
