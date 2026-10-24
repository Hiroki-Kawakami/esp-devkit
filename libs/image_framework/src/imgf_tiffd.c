/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "cmpf_inflate.h"
#include "cmpf_lzw.h"
#include "imgf_alloc.h"
#include "imgf_breader.h"
#include "imgf_decoder.h"
#include "imgf_decoder_internal.h"
#include "imgf_tiffd.h"

#define COMP_NONE      1
#define COMP_LZW       5
#define COMP_DEFLATE   8
#define COMP_DEFLATE_OLD 32946
#define COMP_PACKBITS  32773

#define PHOTO_WHITE_IS_ZERO 0
#define PHOTO_BLACK_IS_ZERO 1
#define PHOTO_RGB           2
#define PHOTO_PALETTE       3

#define ALPHA_NONE           0
#define ALPHA_ASSOCIATED     1
#define ALPHA_UNASSOCIATED   2

#define MAX_ENTRIES 1024

typedef struct {
    uint16_t tag;
    uint16_t type;
    uint32_t count;
    uint8_t  value[4];
} entry_t;

typedef struct {
    imgf_breader_t *br;
    uint32_t        remaining;
    uint8_t         buf[2048];
} chunk_source_t;

typedef struct {
    imgf_decoder_t base;

    imgf_breader_t br;
    uint8_t        br_ready;
    uint32_t       alloc_caps;
    uint8_t        big;

    uint16_t compression;
    uint16_t photometric;
    uint16_t predictor;
    uint16_t spp;
    uint16_t bps;
    uint8_t  alpha;
    uint16_t palette[3][256];

    uint8_t   tiled;
    uint32_t  chunk_w;
    uint32_t  chunk_h;
    uint32_t  across;
    uint32_t  chunk_count;
    uint32_t *offsets;
    uint32_t *counts;
    size_t    raw_bytes;
    int       out_bpp;

    chunk_source_t source;
    cmpf_inflate_t inflate;
    uint8_t       *window;
    cmpf_lzw_t     lzw;
    void          *lzw_work;
    int            pack_run;
    uint8_t        pack_value;

    uint8_t *raw;
    uint8_t *band;
    uint32_t row;
    uint32_t next_chunk;
    uint32_t rows_left;
    uint32_t band_first;
    uint32_t band_rows;
} tiffd_t;

static uint32_t get16(const tiffd_t *d, const uint8_t *p) {
    return d->big ? (uint32_t)((p[0] << 8) | p[1]) : (uint32_t)(p[0] | (p[1] << 8));
}

static uint32_t get32(const tiffd_t *d, const uint8_t *p) {
    return d->big ? ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3]
                  : (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
                        ((uint32_t)p[3] << 24);
}

static int type_size(uint16_t type) {
    switch (type) {
        case 1: return 1;
        case 3: return 2;
        case 4: return 4;
        default: return 0;
    }
}

static bool read_full(imgf_breader_t *br, uint8_t *buf, size_t n) {
    return imgf_breader_read(br, buf, n) == n;
}

static bool read_values(tiffd_t *d, const entry_t *e, uint32_t *out, uint32_t n) {
    const int size = type_size(e->type);
    if (!size || e->count < n) return false;
    const uint8_t *src = e->value;
    uint8_t tmp[4];
    if ((uint64_t)e->count * size > 4 && !imgf_breader_seek(&d->br, get32(d, e->value)))
        return false;
    for (uint32_t i = 0; i < n; i++) {
        if ((uint64_t)e->count * size > 4) {
            if (!read_full(&d->br, tmp, (size_t)size)) return false;
            src = tmp;
        } else {
            src = e->value + i * size;
        }
        out[i] = size == 1 ? src[0] : size == 2 ? get16(d, src) : get32(d, src);
    }
    return true;
}

static bool read_value(tiffd_t *d, const entry_t *e, uint32_t *out) {
    return read_values(d, e, out, 1);
}

static const entry_t *find(const entry_t *entries, int count, uint16_t tag) {
    for (int i = 0; i < count; i++) {
        if (entries[i].tag == tag) return &entries[i];
    }
    return NULL;
}

static uint32_t tag_value(tiffd_t *d, const entry_t *entries, int count, uint16_t tag,
                          uint32_t fallback) {
    uint32_t v;
    const entry_t *e = find(entries, count, tag);
    return e && read_value(d, e, &v) ? v : fallback;
}

static uint32_t *read_array(tiffd_t *d, const entry_t *e, uint32_t n) {
    if (!e || e->count < n) return NULL;
    uint32_t *out = (uint32_t *)imgf_alloc((size_t)n * sizeof *out, d->alloc_caps);
    if (out && !read_values(d, e, out, n)) {
        imgf_free(out);
        return NULL;
    }
    return out;
}

static const uint8_t *chunk_refill(void *user, size_t *n) {
    chunk_source_t *s = (chunk_source_t *)user;
    size_t want = s->remaining < sizeof s->buf ? s->remaining : sizeof s->buf;
    size_t got = want ? imgf_breader_read(s->br, s->buf, want) : 0;
    s->remaining -= (uint32_t)got;
    if (got < want) s->remaining = 0;
    *n = got;
    return s->buf;
}

static bool start_chunk(tiffd_t *d, uint32_t index) {
    if (index >= d->chunk_count || !imgf_breader_seek(&d->br, d->offsets[index])) return false;
    d->source.br = &d->br;
    d->source.remaining = d->counts[index];
    cmpf_src_t src = { chunk_refill, &d->source };
    switch (d->compression) {
        case COMP_LZW: {
            const cmpf_lzw_opts_t opts = { 8, 1, 1 };
            return cmpf_lzw_init(&d->lzw, src, &opts, d->lzw_work);
        }
        case COMP_DEFLATE:
        case COMP_DEFLATE_OLD:
            return cmpf_inflate_init(&d->inflate, src, d->window);
        default:
            d->pack_run = 0;
            return true;
    }
}

static int chunk_byte(tiffd_t *d) {
    if (d->source.remaining == 0) return -1;
    int c = imgf_breader_byte(&d->br);
    if (c < 0) return -1;
    d->source.remaining--;
    return c;
}

static bool unpack_bits(tiffd_t *d, uint8_t *dst, size_t n) {
    size_t i = 0;
    while (i < n) {
        if (d->pack_run > 0) {
            int c = chunk_byte(d);
            if (c < 0) return false;
            dst[i++] = (uint8_t)c;
            d->pack_run--;
        } else if (d->pack_run < 0) {
            dst[i++] = d->pack_value;
            d->pack_run++;
        } else {
            int c = chunk_byte(d);
            if (c < 0) return false;
            if (c < 128) {
                d->pack_run = c + 1;
            } else if (c > 128) {
                int v = chunk_byte(d);
                if (v < 0) return false;
                d->pack_value = (uint8_t)v;
                d->pack_run = -(257 - c);
            }
        }
    }
    return true;
}

static bool read_raw_row(tiffd_t *d) {
    const size_t n = d->raw_bytes;
    switch (d->compression) {
        case COMP_NONE:
            if (d->source.remaining < n || !read_full(&d->br, d->raw, n)) return false;
            d->source.remaining -= (uint32_t)n;
            break;
        case COMP_PACKBITS:
            if (!unpack_bits(d, d->raw, n)) return false;
            break;
        case COMP_LZW:
            if (cmpf_lzw_read(&d->lzw, d->raw, n) != n) return false;
            break;
        default:
            if (cmpf_inflate_read(&d->inflate, d->raw, n) != n) return false;
            break;
    }

    if (d->predictor == 2) {
        const size_t samples = (size_t)d->chunk_w * d->spp;
        if (d->bps == 8) {
            for (size_t i = d->spp; i < samples; i++) d->raw[i] += d->raw[i - d->spp];
        } else {
            const size_t step = (size_t)d->spp * 2;
            const int hi = d->big ? 0 : 1;
            for (size_t i = step; i < samples * 2; i += 2) {
                uint8_t *cur = d->raw + i;
                const uint8_t *prev = cur - step;
                uint32_t v = ((uint32_t)cur[hi] << 8 | cur[1 - hi]) +
                             ((uint32_t)prev[hi] << 8 | prev[1 - hi]);
                cur[hi] = (uint8_t)(v >> 8);
                cur[1 - hi] = (uint8_t)v;
            }
        }
    }
    return true;
}

static uint32_t sample(const tiffd_t *d, size_t i) {
    switch (d->bps) {
        case 8: return d->raw[i];
        case 16: return d->raw[2 * i + (d->big ? 0 : 1)];
        default: {
            size_t bit = i * d->bps;
            return (d->raw[bit >> 3] >> (8 - d->bps - (bit & 7))) & ((1u << d->bps) - 1);
        }
    }
}

static uint8_t scaled(const tiffd_t *d, uint32_t v) {
    if (d->bps >= 8) return (uint8_t)v;
    return (uint8_t)(v * 255 / ((1u << d->bps) - 1));
}

static uint8_t over_white(const tiffd_t *d, uint8_t c, uint8_t a) {
    if (d->alpha == ALPHA_ASSOCIATED) {
        uint32_t v = (uint32_t)c + 255 - a;
        return (uint8_t)(v > 255 ? 255 : v);
    }
    return (uint8_t)((c * a + 255 * (255 - a) + 127) / 255);
}

static void convert(const tiffd_t *d, uint8_t *dst, uint32_t pixels) {
    const uint32_t spp = d->spp;
    for (uint32_t x = 0; x < pixels; x++) {
        const size_t s = (size_t)x * spp;
        if (d->photometric == PHOTO_PALETTE) {
            uint32_t idx = sample(d, s);
            dst[3 * x + 0] = (uint8_t)(d->palette[0][idx] >> 8);
            dst[3 * x + 1] = (uint8_t)(d->palette[1][idx] >> 8);
            dst[3 * x + 2] = (uint8_t)(d->palette[2][idx] >> 8);
        } else if (d->photometric == PHOTO_RGB) {
            for (int c = 0; c < 3; c++) {
                uint8_t v = scaled(d, sample(d, s + c));
                dst[3 * x + c] = d->alpha ? over_white(d, v, scaled(d, sample(d, s + 3))) : v;
            }
        } else {
            uint8_t v = scaled(d, sample(d, s));
            if (d->photometric == PHOTO_WHITE_IS_ZERO) v = (uint8_t)(255 - v);
            dst[x] = d->alpha ? over_white(d, v, scaled(d, sample(d, s + 1))) : v;
        }
    }
}

static imgf_err_t load_band(tiffd_t *d) {
    const uint32_t w = d->base.width;
    const uint32_t tile_row = d->row / d->chunk_h;
    d->band_first = tile_row * d->chunk_h;
    d->band_rows = d->base.height - d->band_first;
    if (d->band_rows > d->chunk_h) d->band_rows = d->chunk_h;
    const size_t stride = (size_t)w * d->out_bpp;

    for (uint32_t tx = 0; tx < d->across; tx++) {
        if (!start_chunk(d, tile_row * d->across + tx)) return IMGF_ERR_DECODE;
        const uint32_t x0 = tx * d->chunk_w;
        const uint32_t pixels = w - x0 < d->chunk_w ? w - x0 : d->chunk_w;
        for (uint32_t r = 0; r < d->band_rows; r++) {
            if (!read_raw_row(d)) return IMGF_ERR_TRUNCATED;
            convert(d, d->band + r * stride + (size_t)x0 * d->out_bpp, pixels);
        }
    }
    return IMGF_OK;
}

/* --- vtable ---------------------------------------------------------------- */

static imgf_err_t tiffd_open(imgf_decoder_t *base, imgf_stream_t src,
                             const imgf_decode_opts_t *opts) {
    tiffd_t *d = (tiffd_t *)base;
    d->alloc_caps = opts->alloc_caps;
    if (!src.seek) return IMGF_ERR_UNSUPPORTED;
    if (!imgf_breader_init(&d->br, src)) return IMGF_ERR_OOM;
    d->br_ready = 1;

    uint8_t hdr[8];
    if (!read_full(&d->br, hdr, sizeof hdr)) return IMGF_ERR_TRUNCATED;
    if (memcmp(hdr, "II*\0", 4) == 0) {
        d->big = 0;
    } else if (memcmp(hdr, "MM\0*", 4) == 0) {
        d->big = 1;
    } else {
        return IMGF_ERR_DECODE;
    }
    uint8_t n[2];
    if (!imgf_breader_seek(&d->br, get32(d, hdr + 4)) || !read_full(&d->br, n, 2))
        return IMGF_ERR_TRUNCATED;
    const int count = (int)get16(d, n);
    if (count == 0 || count > MAX_ENTRIES) return IMGF_ERR_DECODE;
    entry_t *entries = (entry_t *)malloc((size_t)count * sizeof *entries);
    if (!entries) return IMGF_ERR_OOM;
    for (int i = 0; i < count; i++) {
        uint8_t raw[12];
        if (!read_full(&d->br, raw, sizeof raw)) {
            free(entries);
            return IMGF_ERR_TRUNCATED;
        }
        entries[i].tag = (uint16_t)get16(d, raw);
        entries[i].type = (uint16_t)get16(d, raw + 2);
        entries[i].count = get32(d, raw + 4);
        memcpy(entries[i].value, raw + 8, 4);
    }

    imgf_err_t err = IMGF_ERR_DECODE;
    const uint32_t w = tag_value(d, entries, count, 256, 0);
    const uint32_t h = tag_value(d, entries, count, 257, 0);
    d->compression = (uint16_t)tag_value(d, entries, count, 259, COMP_NONE);
    d->photometric = (uint16_t)tag_value(d, entries, count, 262, PHOTO_BLACK_IS_ZERO);
    d->spp = (uint16_t)tag_value(d, entries, count, 277, 1);
    d->bps = (uint16_t)tag_value(d, entries, count, 258, 1);
    d->predictor = (uint16_t)tag_value(d, entries, count, 317, 1);
    if (w == 0 || h == 0 || d->spp == 0) goto done;

    err = IMGF_ERR_UNSUPPORTED;
    if (w > 0xFFFF || h > 0xFFFF) {
        err = IMGF_ERR_TOO_LARGE;
        goto done;
    }
    const entry_t *e = find(entries, count, 258);
    if (e && e->count > 1) {
        uint32_t bps[8];
        uint32_t k = e->count < 8 ? e->count : 8;
        if (!read_values(d, e, bps, k)) goto done;
        for (uint32_t i = 1; i < k; i++) {
            if (bps[i] != bps[0]) goto done;
        }
    }
    if (tag_value(d, entries, count, 284, 1) != 1 && d->spp > 1) goto done;
    if (tag_value(d, entries, count, 339, 1) != 1) goto done;
    if (tag_value(d, entries, count, 266, 1) != 1) goto done;
    if (d->compression != COMP_NONE && d->compression != COMP_LZW &&
        d->compression != COMP_DEFLATE && d->compression != COMP_DEFLATE_OLD &&
        d->compression != COMP_PACKBITS)
        goto done;
    if (d->bps != 1 && d->bps != 2 && d->bps != 4 && d->bps != 8 && d->bps != 16) goto done;
    if (d->predictor != 1 && (d->predictor != 2 || d->bps < 8)) goto done;

    uint16_t colors = 1;
    switch (d->photometric) {
        case PHOTO_WHITE_IS_ZERO:
        case PHOTO_BLACK_IS_ZERO:
            break;
        case PHOTO_RGB:
            if (d->bps < 8) goto done;
            colors = 3;
            break;
        case PHOTO_PALETTE: {
            if (d->bps > 8 || d->spp != 1) goto done;
            const uint32_t entries_n = 1u << d->bps;
            uint32_t *map = read_array(d, find(entries, count, 320), entries_n * 3);
            if (!map) {
                err = IMGF_ERR_DECODE;
                goto done;
            }
            for (uint32_t i = 0; i < entries_n; i++) {
                for (int c = 0; c < 3; c++) d->palette[c][i] = (uint16_t)map[c * entries_n + i];
            }
            imgf_free(map);
            break;
        }
        default:
            goto done;
    }
    if (d->spp < colors) {
        err = IMGF_ERR_DECODE;
        goto done;
    }
    const uint32_t extra = d->spp > colors ? tag_value(d, entries, count, 338, ALPHA_NONE) : 0;
    if (extra == ALPHA_ASSOCIATED || extra == ALPHA_UNASSOCIATED) d->alpha = (uint8_t)extra;

    err = IMGF_ERR_DECODE;
    const entry_t *offsets;
    const entry_t *counts;
    if (find(entries, count, 322)) {
        d->chunk_w = tag_value(d, entries, count, 322, 0);
        const uint32_t th = tag_value(d, entries, count, 323, 0);
        if (d->chunk_w == 0 || th == 0) goto done;
        d->tiled = 1;
        d->chunk_h = th;
        d->across = (w + d->chunk_w - 1) / d->chunk_w;
        d->chunk_count = d->across * ((h + th - 1) / th);
        offsets = find(entries, count, 324);
        counts = find(entries, count, 325);
    } else {
        uint32_t rps = tag_value(d, entries, count, 278, h);
        if (rps == 0 || rps > h) rps = h;
        d->chunk_w = w;
        d->chunk_h = rps;
        d->across = 1;
        d->chunk_count = (h + rps - 1) / rps;
        offsets = find(entries, count, 273);
        counts = find(entries, count, 279);
    }
    d->raw_bytes = ((size_t)d->chunk_w * d->spp * d->bps + 7) / 8;
    d->offsets = read_array(d, offsets, d->chunk_count);
    if (counts) {
        d->counts = read_array(d, counts, d->chunk_count);
    } else if (d->compression == COMP_NONE && d->chunk_count == 1) {
        d->counts = (uint32_t *)imgf_alloc(sizeof *d->counts, d->alloc_caps);
        if (d->counts) d->counts[0] = (uint32_t)(d->raw_bytes * h);
    }
    if (!d->offsets || !d->counts) goto done;

    err = IMGF_ERR_OOM;
    d->out_bpp = colors == 1 && d->photometric != PHOTO_PALETTE ? 1 : 3;
    d->raw = (uint8_t *)imgf_alloc(d->raw_bytes, d->alloc_caps);
    if (!d->raw) goto done;
    if (d->compression == COMP_LZW && !(d->lzw_work = imgf_alloc(CMPF_LZW_WORK_BYTES, 0)))
        goto done;
    if ((d->compression == COMP_DEFLATE || d->compression == COMP_DEFLATE_OLD) &&
        !(d->window = (uint8_t *)imgf_alloc(CMPF_INFLATE_WINDOW_BYTES, 0)))
        goto done;
    if (d->tiled) {
        d->band = (uint8_t *)imgf_alloc((size_t)d->chunk_h * w * d->out_bpp, d->alloc_caps);
        if (!d->band) goto done;
    }

    d->base.width  = (uint16_t)w;
    d->base.height = (uint16_t)h;
    d->base.pixfmt = d->out_bpp == 3 ? IMGF_PIX_RGB888 : IMGF_PIX_GRAY8;
    err = IMGF_OK;
done:
    free(entries);
    return err;
}

static bool tiffd_next_row(imgf_decoder_t *base, uint8_t *dst) {
    tiffd_t *d = (tiffd_t *)base;
    if (d->row >= base->height) return false;

    if (d->tiled) {
        if (d->row >= d->band_first + d->band_rows || d->band_rows == 0) {
            imgf_err_t err = load_band(d);
            if (err != IMGF_OK) {
                base->last_error = err;
                return false;
            }
        }
        const size_t stride = (size_t)base->width * d->out_bpp;
        memcpy(dst, d->band + (d->row - d->band_first) * stride, stride);
        d->row++;
        return true;
    }

    if (d->rows_left == 0) {
        if (!start_chunk(d, d->next_chunk++)) {
            base->last_error = IMGF_ERR_DECODE;
            return false;
        }
        d->rows_left = d->chunk_h;
    }
    if (!read_raw_row(d)) {
        base->last_error = IMGF_ERR_TRUNCATED;
        return false;
    }
    convert(d, dst, base->width);
    d->rows_left--;
    d->row++;
    return true;
}

static void tiffd_destroy(imgf_decoder_t *base) {
    tiffd_t *d = (tiffd_t *)base;
    if (d->br_ready) imgf_breader_deinit(&d->br);
    imgf_free(d->offsets);
    imgf_free(d->counts);
    imgf_free(d->window);
    imgf_free(d->lzw_work);
    imgf_free(d->raw);
    imgf_free(d->band);
    free(d);
}

static const imgf_decoder_vtable_t k_tiffd_vt = {
    tiffd_open, tiffd_next_row, tiffd_destroy,
};

imgf_decoder_t *imgf_tiffd_create(void) {
    tiffd_t *d = (tiffd_t *)calloc(1, sizeof *d);
    if (!d) return NULL;
    d->base.vt     = &k_tiffd_vt;
    d->base.pixfmt = IMGF_PIX_RGB888;
    return &d->base;
}
