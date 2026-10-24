/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_bmpd.h"
#include "imgf_breader.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"

#define BI_RGB            0
#define BI_RLE8           1
#define BI_RLE4           2
#define BI_BITFIELDS      3
#define BI_ALPHABITFIELDS 6

#define CHUNK_BYTES 32768u

typedef struct {
    uint32_t mask;
    uint8_t  shift;
    uint8_t  bits;
} channel_t;

typedef struct {
    imgf_decoder_t base;

    imgf_breader_t br;
    uint8_t        br_ready;
    uint32_t       alloc_caps;

    uint32_t data_offset;
    uint32_t stride;
    uint16_t bpp;
    uint32_t compression;
    uint8_t  bottom_up;
    channel_t channels[3];

    uint8_t  palette[256][3];
    int      palette_count;

    uint32_t row;
    uint8_t *chunk;
    uint32_t chunk_rows;
    uint32_t chunk_first;
    uint32_t chunk_count;

    uint8_t *indices;
} bmpd_t;

static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static bool read_full(imgf_breader_t *br, uint8_t *buf, size_t n) {
    return imgf_breader_read(br, buf, n) == n;
}

static void set_channel(channel_t *c, uint32_t mask) {
    c->mask = mask;
    c->shift = 0;
    c->bits = 0;
    if (!mask) return;
    while (!(mask & 1)) {
        mask >>= 1;
        c->shift++;
    }
    while (mask & 1) {
        mask >>= 1;
        c->bits++;
    }
}

static uint8_t channel_value(const channel_t *c, uint32_t px) {
    if (!c->bits) return 0;
    uint32_t v = (px & c->mask) >> c->shift;
    if (c->bits >= 8) return (uint8_t)(v >> (c->bits - 8));
    return (uint8_t)(v * 255 / ((1u << c->bits) - 1));
}

static void put_index(const bmpd_t *d, uint8_t *dst, int idx) {
    if (idx >= d->palette_count) idx = 0;
    dst[0] = d->palette[idx][0];
    dst[1] = d->palette[idx][1];
    dst[2] = d->palette[idx][2];
}

static void convert_row(const bmpd_t *d, const uint8_t *src, uint8_t *dst) {
    const int w = d->base.width;
    switch (d->bpp) {
        case 1:
        case 4:
        case 8: {
            const int bpp = d->bpp;
            const int maxv = (1 << bpp) - 1;
            for (int x = 0; x < w; x++) {
                int bitpos = x * bpp;
                put_index(d, dst + 3 * x, (src[bitpos >> 3] >> (8 - bpp - (bitpos & 7))) & maxv);
            }
            break;
        }
        case 16:
            for (int x = 0; x < w; x++) {
                uint32_t px = le16(src + 2 * x);
                for (int c = 0; c < 3; c++) dst[3 * x + c] = channel_value(&d->channels[c], px);
            }
            break;
        case 24:
            for (int x = 0; x < w; x++) {
                dst[3 * x + 0] = src[3 * x + 2];
                dst[3 * x + 1] = src[3 * x + 1];
                dst[3 * x + 2] = src[3 * x + 0];
            }
            break;
        case 32:
            for (int x = 0; x < w; x++) {
                uint32_t px = le32(src + 4 * x);
                for (int c = 0; c < 3; c++) dst[3 * x + c] = channel_value(&d->channels[c], px);
            }
            break;
        default:
            break;
    }
}

static bool load_chunk(bmpd_t *d, uint32_t stored) {
    const uint32_t h = d->base.height;
    uint32_t first = stored;
    uint32_t count = d->chunk_rows;
    if (d->bottom_up) {
        first = stored + 1 >= count ? stored + 1 - count : 0;
        count = stored - first + 1;
    } else if (first + count > h) {
        count = h - first;
    }
    const size_t at = (size_t)d->data_offset + (size_t)first * d->stride;
    if (!imgf_breader_seek(&d->br, at)) return false;
    if (!read_full(&d->br, d->chunk, (size_t)count * d->stride)) return false;
    d->chunk_first = first;
    d->chunk_count = count;
    return true;
}

static void rle_put(bmpd_t *d, uint32_t *x, uint32_t y, uint8_t idx) {
    if (*x < d->base.width && y < d->base.height) {
        d->indices[(size_t)y * d->base.width + *x] = idx;
    }
    (*x)++;
}

static imgf_err_t expand_rle(bmpd_t *d) {
    const size_t pixels = (size_t)d->base.width * d->base.height;
    d->indices = (uint8_t *)imgf_alloc(pixels, d->alloc_caps);
    if (!d->indices) return IMGF_ERR_OOM;
    memset(d->indices, 0, pixels);
    if (!imgf_breader_seek(&d->br, d->data_offset)) return IMGF_ERR_TRUNCATED;

    const bool four = d->compression == BI_RLE4;
    uint32_t x = 0, y = 0;
    while (y < d->base.height) {
        int count = imgf_breader_byte(&d->br);
        int value = imgf_breader_byte(&d->br);
        if (count < 0 || value < 0) return IMGF_ERR_TRUNCATED;
        if (count > 0) {
            for (int i = 0; i < count; i++) {
                uint8_t idx = four ? (uint8_t)(i & 1 ? value & 0x0F : value >> 4) : (uint8_t)value;
                rle_put(d, &x, y, idx);
            }
        } else if (value == 0) {
            x = 0;
            y++;
        } else if (value == 1) {
            break;
        } else if (value == 2) {
            int dx = imgf_breader_byte(&d->br);
            int dy = imgf_breader_byte(&d->br);
            if (dx < 0 || dy < 0) return IMGF_ERR_TRUNCATED;
            x += (uint32_t)dx;
            y += (uint32_t)dy;
        } else {
            int bytes = four ? (value + 1) / 2 : value;
            int b = 0;
            for (int i = 0; i < value; i++) {
                if (!four || !(i & 1)) {
                    b = imgf_breader_byte(&d->br);
                    if (b < 0) return IMGF_ERR_TRUNCATED;
                }
                uint8_t idx = four ? (uint8_t)(i & 1 ? b & 0x0F : b >> 4) : (uint8_t)b;
                rle_put(d, &x, y, idx);
            }
            if ((bytes & 1) && imgf_breader_byte(&d->br) < 0) return IMGF_ERR_TRUNCATED;
        }
    }
    return IMGF_OK;
}

/* --- vtable ---------------------------------------------------------------- */

static imgf_err_t bmpd_open(imgf_decoder_t *base, imgf_stream_t src,
                            const imgf_decode_opts_t *opts) {
    bmpd_t *d = (bmpd_t *)base;
    d->alloc_caps = opts->alloc_caps;
    if (!imgf_breader_init(&d->br, src)) return IMGF_ERR_OOM;
    d->br_ready = 1;

    uint8_t fh[18];
    if (!read_full(&d->br, fh, sizeof fh)) return IMGF_ERR_TRUNCATED;
    if (fh[0] != 'B' || fh[1] != 'M') return IMGF_ERR_DECODE;
    d->data_offset = le32(fh + 10);
    const uint32_t info_size = le32(fh + 14);

    uint8_t ih[124];
    int32_t w, h;
    int palette_entry = 4;
    uint32_t colors_used = 0;
    if (info_size == 12) {
        if (!read_full(&d->br, ih, 8)) return IMGF_ERR_TRUNCATED;
        w = le16(ih);
        h = le16(ih + 2);
        d->bpp = le16(ih + 6);
        d->compression = BI_RGB;
        palette_entry = 3;
    } else if (info_size == 40 || info_size == 52 || info_size == 56 || info_size == 108 ||
               info_size == 124) {
        if (!read_full(&d->br, ih, info_size - 4)) return IMGF_ERR_TRUNCATED;
        w = (int32_t)le32(ih);
        h = (int32_t)le32(ih + 4);
        d->bpp = le16(ih + 10);
        d->compression = le32(ih + 12);
        colors_used = le32(ih + 28);
    } else {
        return IMGF_ERR_UNSUPPORTED;
    }

    if (w <= 0 || h == 0 || h == INT32_MIN) return IMGF_ERR_DECODE;
    d->bottom_up = h > 0;
    if (h < 0) h = -h;
    if (w > 0xFFFF || h > 0xFFFF) return IMGF_ERR_TOO_LARGE;

    switch (d->compression) {
        case BI_RGB:
            if (d->bpp != 1 && d->bpp != 4 && d->bpp != 8 && d->bpp != 16 && d->bpp != 24 &&
                d->bpp != 32)
                return IMGF_ERR_UNSUPPORTED;
            break;
        case BI_RLE8:
            if (d->bpp != 8 || !d->bottom_up) return IMGF_ERR_DECODE;
            break;
        case BI_RLE4:
            if (d->bpp != 4 || !d->bottom_up) return IMGF_ERR_DECODE;
            break;
        case BI_BITFIELDS:
        case BI_ALPHABITFIELDS:
            if (d->bpp != 16 && d->bpp != 32) return IMGF_ERR_DECODE;
            break;
        default:
            return IMGF_ERR_UNSUPPORTED;
    }

    if (d->compression == BI_BITFIELDS || d->compression == BI_ALPHABITFIELDS) {
        uint8_t masks[12];
        if (info_size == 40) {
            if (!read_full(&d->br, masks, sizeof masks)) return IMGF_ERR_TRUNCATED;
            if (d->compression == BI_ALPHABITFIELDS && !read_full(&d->br, ih, 4))
                return IMGF_ERR_TRUNCATED;
        } else {
            memcpy(masks, ih + 36, sizeof masks);
        }
        for (int c = 0; c < 3; c++) set_channel(&d->channels[c], le32(masks + 4 * c));
    } else if (d->bpp == 16) {
        set_channel(&d->channels[0], 0x7C00);
        set_channel(&d->channels[1], 0x03E0);
        set_channel(&d->channels[2], 0x001F);
    } else if (d->bpp == 32) {
        set_channel(&d->channels[0], 0x00FF0000);
        set_channel(&d->channels[1], 0x0000FF00);
        set_channel(&d->channels[2], 0x000000FF);
    }

    if (d->bpp <= 8) {
        uint32_t count = colors_used ? colors_used : (1u << d->bpp);
        if (count > 256) count = 256;
        for (uint32_t i = 0; i < count; i++) {
            uint8_t e[4];
            if (!read_full(&d->br, e, (size_t)palette_entry)) return IMGF_ERR_TRUNCATED;
            d->palette[i][0] = e[2];
            d->palette[i][1] = e[1];
            d->palette[i][2] = e[0];
        }
        d->palette_count = (int)count;
    }

    const uint64_t stride = ((uint64_t)w * d->bpp + 31) / 32 * 4;
    d->stride = (uint32_t)stride;
    d->base.width  = (uint16_t)w;
    d->base.height = (uint16_t)h;
    d->base.pixfmt = IMGF_PIX_RGB888;
    d->row = 0;

    if (d->compression == BI_RLE8 || d->compression == BI_RLE4) return IMGF_OK;
    if (d->bottom_up && !src.seek) return IMGF_ERR_UNSUPPORTED;
    d->chunk_rows = (uint32_t)(CHUNK_BYTES / stride);
    if (d->chunk_rows == 0) d->chunk_rows = 1;
    if (d->chunk_rows > (uint32_t)h) d->chunk_rows = (uint32_t)h;
    d->chunk = (uint8_t *)imgf_alloc((size_t)d->chunk_rows * d->stride, d->alloc_caps);
    if (!d->chunk) return IMGF_ERR_OOM;
    return IMGF_OK;
}

static bool bmpd_next_row(imgf_decoder_t *base, uint8_t *dst) {
    bmpd_t *d = (bmpd_t *)base;
    if (d->row >= base->height) return false;
    const uint32_t stored = d->bottom_up ? base->height - 1 - d->row : d->row;

    if (d->compression == BI_RLE8 || d->compression == BI_RLE4) {
        if (!d->indices) {
            imgf_err_t err = expand_rle(d);
            if (err != IMGF_OK) {
                base->last_error = err;
                return false;
            }
        }
        const uint8_t *line = d->indices + (size_t)stored * base->width;
        for (int x = 0; x < base->width; x++) put_index(d, dst + 3 * x, line[x]);
        d->row++;
        return true;
    }

    if (!d->chunk_count || stored < d->chunk_first || stored >= d->chunk_first + d->chunk_count) {
        if (!load_chunk(d, stored)) {
            base->last_error = IMGF_ERR_TRUNCATED;
            return false;
        }
    }
    convert_row(d, d->chunk + (size_t)(stored - d->chunk_first) * d->stride, dst);
    d->row++;
    return true;
}

static void bmpd_destroy(imgf_decoder_t *base) {
    bmpd_t *d = (bmpd_t *)base;
    if (d->br_ready) imgf_breader_deinit(&d->br);
    imgf_free(d->chunk);
    imgf_free(d->indices);
    free(d);
}

static const imgf_decoder_vtable_t k_bmpd_vt = {
    bmpd_open, bmpd_next_row, bmpd_destroy,
};

imgf_decoder_t *imgf_bmpd_create(void) {
    bmpd_t *d = (bmpd_t *)calloc(1, sizeof *d);
    if (!d) return NULL;
    d->base.vt     = &k_bmpd_vt;
    d->base.pixfmt = IMGF_PIX_RGB888;
    return &d->base;
}
