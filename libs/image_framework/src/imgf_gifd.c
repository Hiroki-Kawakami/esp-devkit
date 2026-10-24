/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "cmpf_lzw.h"
#include "imgf_alloc.h"
#include "imgf_breader.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"
#include "imgf_gifd.h"

typedef struct {
    imgf_breader_t *br;
    uint8_t         remaining;
    uint8_t         done;
    uint8_t         buf[255];
} block_source_t;

typedef struct {
    imgf_decoder_t base;

    imgf_breader_t br;
    uint8_t        br_ready;
    uint32_t       alloc_caps;

    block_source_t blocks;
    cmpf_lzw_t     lzw;
    void          *lzw_work;

    uint8_t  palette[256][3];
    int      palette_count;
    int      transparent;

    uint16_t left;
    uint16_t top;
    uint16_t frame_w;
    uint16_t frame_h;
    uint8_t  interlaced;

    uint32_t row;
    uint8_t *line;
    uint8_t *indices;
} gifd_t;

static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }

static bool read_full(imgf_breader_t *br, uint8_t *buf, size_t n) {
    return imgf_breader_read(br, buf, n) == n;
}

static bool skip_blocks(imgf_breader_t *br) {
    uint8_t tmp[255];
    for (;;) {
        int len = imgf_breader_byte(br);
        if (len < 0) return false;
        if (len == 0) return true;
        if (!read_full(br, tmp, (size_t)len)) return false;
    }
}

static bool read_palette(gifd_t *d, uint8_t flags) {
    int count = 2 << (flags & 7);
    for (int i = 0; i < count; i++) {
        if (!read_full(&d->br, d->palette[i], 3)) return false;
    }
    d->palette_count = count;
    return true;
}

static const uint8_t *block_refill(void *user, size_t *n) {
    block_source_t *s = (block_source_t *)user;
    if (s->done) {
        *n = 0;
        return NULL;
    }
    int len = imgf_breader_byte(s->br);
    if (len <= 0) {
        s->done = 1;
        *n = 0;
        return NULL;
    }
    *n = imgf_breader_read(s->br, s->buf, (size_t)len);
    if (*n < (size_t)len) s->done = 1;
    return s->buf;
}

static bool read_frame_row(gifd_t *d, uint8_t *dst) {
    return cmpf_lzw_read(&d->lzw, dst, d->frame_w) == d->frame_w;
}

static imgf_err_t expand_interlaced(gifd_t *d) {
    static const uint8_t kStart[4] = {0, 4, 2, 1};
    static const uint8_t kStep[4] = {8, 8, 4, 2};
    d->indices = (uint8_t *)imgf_alloc((size_t)d->frame_w * d->frame_h, d->alloc_caps);
    if (!d->indices) return IMGF_ERR_OOM;
    for (int pass = 0; pass < 4; pass++) {
        for (uint32_t y = kStart[pass]; y < d->frame_h; y += kStep[pass]) {
            if (!read_frame_row(d, d->indices + (size_t)y * d->frame_w)) {
                return cmpf_lzw_failed(&d->lzw) ? IMGF_ERR_DECODE : IMGF_ERR_TRUNCATED;
            }
        }
    }
    return IMGF_OK;
}

/* --- vtable ---------------------------------------------------------------- */

static imgf_err_t gifd_open(imgf_decoder_t *base, imgf_stream_t src,
                            const imgf_decode_opts_t *opts) {
    gifd_t *d = (gifd_t *)base;
    d->alloc_caps = opts->alloc_caps;
    if (!imgf_breader_init(&d->br, src)) return IMGF_ERR_OOM;
    d->br_ready = 1;

    uint8_t hdr[13];
    if (!read_full(&d->br, hdr, sizeof hdr)) return IMGF_ERR_TRUNCATED;
    if (memcmp(hdr, "GIF87a", 6) != 0 && memcmp(hdr, "GIF89a", 6) != 0) return IMGF_ERR_DECODE;
    uint32_t w = le16(hdr + 6);
    uint32_t h = le16(hdr + 8);
    if ((hdr[10] & 0x80) && !read_palette(d, hdr[10])) return IMGF_ERR_TRUNCATED;

    d->transparent = -1;
    for (;;) {
        int tag = imgf_breader_byte(&d->br);
        if (tag < 0) return IMGF_ERR_TRUNCATED;
        if (tag == 0x2C) break;
        if (tag != 0x21) return IMGF_ERR_DECODE;
        int label = imgf_breader_byte(&d->br);
        if (label < 0) return IMGF_ERR_TRUNCATED;
        if (label == 0xF9) {
            uint8_t gce[255];
            int len = imgf_breader_byte(&d->br);
            if (len < 0 || !read_full(&d->br, gce, (size_t)len)) return IMGF_ERR_TRUNCATED;
            d->transparent = len >= 4 && (gce[0] & 1) ? gce[3] : -1;
        }
        if (!skip_blocks(&d->br)) return IMGF_ERR_TRUNCATED;
    }

    uint8_t desc[9];
    if (!read_full(&d->br, desc, sizeof desc)) return IMGF_ERR_TRUNCATED;
    d->left = le16(desc);
    d->top = le16(desc + 2);
    d->frame_w = le16(desc + 4);
    d->frame_h = le16(desc + 6);
    d->interlaced = (desc[8] & 0x40) != 0;
    if ((desc[8] & 0x80) && !read_palette(d, desc[8])) return IMGF_ERR_TRUNCATED;
    if (d->palette_count == 0) {
        for (int i = 0; i < 256; i++) memset(d->palette[i], i, 3);
        d->palette_count = 256;
    }

    if (d->left + (uint32_t)d->frame_w > w) w = d->left + (uint32_t)d->frame_w;
    if (d->top + (uint32_t)d->frame_h > h) h = d->top + (uint32_t)d->frame_h;
    if (w == 0 || h == 0) return IMGF_ERR_DECODE;
    if (w > 0xFFFF || h > 0xFFFF) return IMGF_ERR_TOO_LARGE;

    int min_bits = imgf_breader_byte(&d->br);
    if (min_bits < 0) return IMGF_ERR_TRUNCATED;
    if (min_bits < 2 || min_bits > 8) return IMGF_ERR_DECODE;

    d->lzw_work = imgf_alloc(CMPF_LZW_WORK_BYTES, 0);
    d->line = (uint8_t *)imgf_alloc(d->frame_w ? d->frame_w : 1, 0);
    if (!d->lzw_work || !d->line) return IMGF_ERR_OOM;
    d->blocks.br = &d->br;
    const cmpf_lzw_opts_t lzw = { (uint8_t)min_bits, 0, 0 };
    cmpf_src_t bsrc = { block_refill, &d->blocks };
    if (!cmpf_lzw_init(&d->lzw, bsrc, &lzw, d->lzw_work)) return IMGF_ERR_DECODE;

    d->base.width  = (uint16_t)w;
    d->base.height = (uint16_t)h;
    d->base.pixfmt = IMGF_PIX_RGB888;
    d->row = 0;
    return IMGF_OK;
}

static bool gifd_next_row(imgf_decoder_t *base, uint8_t *dst) {
    gifd_t *d = (gifd_t *)base;
    if (d->row >= base->height) return false;
    const uint32_t y = d->row;
    memset(dst, 0xFF, (size_t)base->width * 3);

    if (y >= d->top && y < (uint32_t)d->top + d->frame_h) {
        const uint32_t fy = y - d->top;
        const uint8_t *line = d->line;
        if (d->interlaced) {
            if (!d->indices) {
                imgf_err_t err = expand_interlaced(d);
                if (err != IMGF_OK) {
                    base->last_error = err;
                    return false;
                }
            }
            line = d->indices + (size_t)fy * d->frame_w;
        } else if (!read_frame_row(d, d->line)) {
            base->last_error = cmpf_lzw_failed(&d->lzw) ? IMGF_ERR_DECODE : IMGF_ERR_TRUNCATED;
            return false;
        }
        uint8_t *out = dst + (size_t)d->left * 3;
        for (uint32_t x = 0; x < d->frame_w; x++, out += 3) {
            int idx = line[x];
            if (idx == d->transparent) continue;
            if (idx >= d->palette_count) idx = 0;
            out[0] = d->palette[idx][0];
            out[1] = d->palette[idx][1];
            out[2] = d->palette[idx][2];
        }
    }
    d->row++;
    return true;
}

static void gifd_destroy(imgf_decoder_t *base) {
    gifd_t *d = (gifd_t *)base;
    if (d->br_ready) imgf_breader_deinit(&d->br);
    imgf_free(d->lzw_work);
    imgf_free(d->line);
    imgf_free(d->indices);
    free(d);
}

static const imgf_decoder_vtable_t k_gifd_vt = {
    gifd_open, gifd_next_row, gifd_destroy,
};

imgf_decoder_t *imgf_gifd_create(void) {
    gifd_t *d = (gifd_t *)calloc(1, sizeof *d);
    if (!d) return NULL;
    d->base.vt     = &k_gifd_vt;
    d->base.pixfmt = IMGF_PIX_RGB888;
    return &d->base;
}
