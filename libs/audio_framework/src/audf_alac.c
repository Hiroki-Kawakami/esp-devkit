/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_alac.h"
#include <stdbool.h>
#include <string.h>
#include "audf_alloc.h"
#include "audf_codec_internal.h"

#define ALAC_MAX_BITS 24
#define ALAC_MAX_CHANNELS 8
#define ALAC_MAX_FRAME 65536
#define ALAC_MAX_ORDER 31

enum {
    ID_SCE,
    ID_CPE,
    ID_CCE,
    ID_LFE,
    ID_DSE,
    ID_PCE,
    ID_FIL,
    ID_END,
};

#define QBSHIFT 9
#define QB (1u << QBSHIFT)
#define MMULSHIFT 2
#define MDENSHIFT (QBSHIFT - MMULSHIFT - 1)
#define MOFF (1u << (MDENSHIFT - 2))
#define BITOFF 24
#define MAX_PREFIX 9
#define RUN_ESCAPE_BITS 16
#define MEAN_CLAMP 0xffffu

typedef struct {
    audf_decoder_t base;
    uint32_t rate;
    uint8_t  bits;
    uint8_t  pb;
    uint8_t  mb;
    uint8_t  kb;
    int32_t *predictor;
    int32_t *mix[2];
} alac_decoder_t;

typedef struct {
    const uint8_t *p;
    size_t len;
    size_t pos;
} bits_t;

typedef struct {
    uint32_t frames;
    int      mix_bits;
    int      mix_res;
    int      shift;
} element_t;

typedef struct {
    uint32_t mode;
    uint32_t den_shift;
    uint32_t pb_factor;
    uint32_t order;
    int16_t  coefs[ALAC_MAX_ORDER];
} channel_t;

static uint32_t be32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}

static uint32_t peek32(const bits_t *b) {
    const size_t byte = b->pos >> 3;
    uint64_t v = 0;
    for (size_t i = 0; i < 5; i++) v = v << 8 | (byte + i < b->len ? b->p[byte + i] : 0);
    return (uint32_t)(v >> (8 - (b->pos & 7)));
}

static uint32_t read_bits(bits_t *b, int n) {
    if (!n) return 0;
    const uint32_t v = peek32(b) >> (32 - n);
    b->pos += (size_t)n;
    return v;
}

static int32_t read_signed(bits_t *b, int n) {
    const uint32_t v = read_bits(b, n) << (32 - n);
    return (int32_t)v >> (32 - n);
}

static bool overrun(const bits_t *b) {
    return b->pos > b->len * 8;
}

static uint32_t read_golomb(bits_t *b, uint32_t m, int k, int escape_bits) {
    const uint32_t stream = peek32(b);
    const uint32_t prefix = stream == UINT32_MAX ? 32 : (uint32_t)__builtin_clz(~stream);
    if (prefix >= MAX_PREFIX) {
        b->pos += MAX_PREFIX;
        return read_bits(b, escape_bits);
    }
    b->pos += prefix + 1;
    const uint32_t v = peek32(b) >> (32 - k);
    if (v < 2) {
        b->pos += (size_t)k - 1;
        return prefix * m;
    }
    b->pos += (size_t)k;
    return prefix * m + v - 1;
}

static esp_err_t read_residual(const alac_decoder_t *d, bits_t *b, int32_t *out, uint32_t n, uint32_t pb,
                               int escape_bits) {
    const uint32_t kb = d->kb;
    const uint32_t wb = (1u << kb) - 1;
    uint32_t mb = d->mb;
    uint32_t zmode = 0;
    for (uint32_t c = 0; c < n;) {
        if (b->pos >= b->len * 8) return ESP_ERR_INVALID_SIZE;
        uint32_t k = 31u - (uint32_t)__builtin_clz((mb >> QBSHIFT) + 3);
        if (k > kb) k = kb;
        const uint32_t v = read_golomb(b, (1u << k) - 1, (int)k, escape_bits);
        const uint32_t nd = v + zmode;
        const uint32_t half = (nd + 1) >> 1;
        out[c++] = (int32_t)(nd & 1 ? 0u - half : half);
        mb = pb * nd + mb - ((pb * mb) >> QBSHIFT);
        if (v > MEAN_CLAMP) mb = MEAN_CLAMP;
        zmode = 0;
        if ((mb << MMULSHIFT) < QB && c < n) {
            zmode = 1;
            const uint32_t lead = mb ? (uint32_t)__builtin_clz(mb) : 32;
            k = lead - BITOFF + ((mb + MOFF) >> MDENSHIFT);
            if (k < 1 || k > 31) return ESP_ERR_INVALID_RESPONSE;
            const uint32_t run = read_golomb(b, ((1u << k) - 1) & wb, (int)k, RUN_ESCAPE_BITS);
            if (run > n - c) return ESP_ERR_INVALID_RESPONSE;
            for (uint32_t j = 0; j < run; j++) out[c++] = 0;
            if (run >= 65535) zmode = 0;
            mb = 0;
        }
    }
    return overrun(b) ? ESP_ERR_INVALID_SIZE : ESP_OK;
}

static int32_t sign_of(int32_t v) {
    return (v > 0) - (v < 0);
}

static void predict(const int32_t *in, int32_t *out, uint32_t n, int16_t *coefs, uint32_t order, int bits,
                    int den_shift) {
    const int clip = 32 - bits;
#define CLIP(v) ((int32_t)((uint32_t)(v) << clip) >> clip)
    out[0] = in[0];
    if (order == 0) {
        if (in != out) memcpy(out + 1, in + 1, (n - 1) * sizeof(int32_t));
        return;
    }
    if (order == 31) {
        int32_t prev = out[0];
        for (uint32_t j = 1; j < n; j++) {
            prev = CLIP((uint32_t)in[j] + (uint32_t)prev);
            out[j] = prev;
        }
        return;
    }
    const uint32_t warm = order + 1 < n ? order + 1 : n;
    for (uint32_t j = 1; j < warm; j++) out[j] = CLIP((uint32_t)in[j] + (uint32_t)out[j - 1]);

    const uint32_t half = den_shift ? 1u << (den_shift - 1) : 0;
    for (uint32_t j = order + 1; j < n; j++) {
        const int32_t *x = out + j - 1;
        const int32_t top = out[j - order - 1];
        uint32_t sum = 0;
        for (uint32_t k = 0; k < order; k++) sum += (uint32_t)coefs[k] * (uint32_t)(x[-(int32_t)k] - top);

        const int32_t del = in[j];
        out[j] = CLIP((uint32_t)del + (uint32_t)top + (uint32_t)((int32_t)(sum + half) >> den_shift));

        int32_t rest = del;
        if (del > 0) {
            for (int k = (int)order - 1; k >= 0; k--) {
                const int32_t dd = top - x[-k];
                const int32_t sgn = sign_of(dd);
                coefs[k] = (int16_t)(coefs[k] - sgn);
                rest -= ((int32_t)order - k) * ((sgn * dd) >> den_shift);
                if (rest <= 0) break;
            }
        } else if (del < 0) {
            for (int k = (int)order - 1; k >= 0; k--) {
                const int32_t dd = top - x[-k];
                const int32_t sgn = sign_of(dd);
                coefs[k] = (int16_t)(coefs[k] + sgn);
                rest -= ((int32_t)order - k) * ((-sgn * dd) >> den_shift);
                if (rest >= 0) break;
            }
        }
    }
#undef CLIP
}

static esp_err_t read_element(alac_decoder_t *d, bits_t *b, uint8_t count, element_t *e) {
    read_bits(b, 4);
    if (read_bits(b, 12)) return ESP_ERR_INVALID_RESPONSE;
    const uint32_t head = read_bits(b, 4);
    const uint32_t shifted = (head >> 1) & 3;
    if (shifted == 3) return ESP_ERR_INVALID_RESPONSE;
    e->frames = (uint32_t)d->base.max_frames;
    if (head & 8) e->frames = read_bits(b, 32);
    if (!e->frames) return ESP_ERR_INVALID_RESPONSE;
    if (e->frames > d->base.max_frames) return ESP_ERR_INVALID_SIZE;
    e->shift = (int)shifted * 8;
    if (e->shift >= d->bits) return ESP_ERR_INVALID_RESPONSE;
    e->mix_bits = 0;
    e->mix_res = 0;

    if (head & 1) {
        const int bits = count == 1 ? d->bits - e->shift : d->bits;
        for (uint32_t i = 0; i < e->frames; i++) {
            for (uint8_t c = 0; c < count; c++) d->mix[c][i] = read_signed(b, bits);
        }
        e->shift = 0;
        return overrun(b) ? ESP_ERR_INVALID_SIZE : ESP_OK;
    }

    if (!d->kb) return ESP_ERR_INVALID_RESPONSE;
    e->mix_bits = (int)read_bits(b, 8);
    e->mix_res = (int8_t)read_bits(b, 8);
    if (e->mix_bits > 31) return ESP_ERR_INVALID_RESPONSE;
    channel_t ch[2];
    for (uint8_t c = 0; c < count; c++) {
        uint32_t h = read_bits(b, 8);
        ch[c].mode = h >> 4;
        ch[c].den_shift = h & 15;
        h = read_bits(b, 8);
        ch[c].pb_factor = h >> 5;
        ch[c].order = h & 31;
        for (uint32_t i = 0; i < ch[c].order; i++) ch[c].coefs[i] = (int16_t)read_bits(b, 16);
    }
    bits_t low = *b;
    b->pos += (size_t)e->shift * count * e->frames;

    const int bits = d->bits - e->shift + count - 1;
    for (uint8_t c = 0; c < count; c++) {
        const esp_err_t err =
            read_residual(d, b, d->predictor, e->frames, d->pb * ch[c].pb_factor / 4, bits);
        if (err != ESP_OK) return err;
        if (ch[c].mode) predict(d->predictor, d->predictor, e->frames, NULL, 31, bits, 0);
        predict(d->predictor, d->mix[c], e->frames, ch[c].coefs, ch[c].order, bits, (int)ch[c].den_shift);
    }
    if (e->shift) {
        uint16_t *out = (uint16_t *)d->predictor;
        for (uint32_t i = 0; i < e->frames * count; i++) out[i] = (uint16_t)read_bits(&low, e->shift);
    }
    return ESP_OK;
}

static void write_sample(const alac_decoder_t *d, void *pcm, size_t index, int32_t v) {
    if (d->base.fmt == AUDF_FMT_S16) {
        ((int16_t *)pcm)[index] = (int16_t)((uint32_t)v << (16 - d->bits));
    } else {
        ((int32_t *)pcm)[index] = (int32_t)((uint32_t)v << (24 - d->bits));
    }
}

static void write_element(const alac_decoder_t *d, const element_t *e, uint8_t count, void *pcm, uint8_t first) {
    const uint8_t channels = d->base.channels;
    const uint16_t *low = (const uint16_t *)d->predictor;
    const int32_t *u = d->mix[0];
    const int32_t *v = d->mix[1];
    for (uint32_t i = 0; i < e->frames; i++) {
        int32_t s[2] = { u[i], 0 };
        if (count == 2) {
            s[1] = v[i];
            if (e->mix_res) {
                s[0] = u[i] + v[i] - ((e->mix_res * v[i]) >> e->mix_bits);
                s[1] = s[0] - v[i];
            }
        }
        for (uint8_t c = 0; c < count; c++) {
            int32_t x = s[c];
            if (e->shift) x = (int32_t)((uint32_t)x << e->shift | low[i * count + c]);
            write_sample(d, pcm, (size_t)i * channels + first + c, x);
        }
    }
}

static esp_err_t decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    alac_decoder_t *d = (alac_decoder_t *)dec;
    bits_t b = { .p = frame, .len = len };
    uint8_t ch = 0;
    uint32_t total = 0;
    while (ch < dec->channels) {
        if (b.pos >= len * 8) return ESP_ERR_INVALID_SIZE;
        const uint32_t tag = read_bits(&b, 3);
        switch (tag) {
        case ID_SCE:
        case ID_LFE:
        case ID_CPE: {
            const uint8_t count = tag == ID_CPE ? 2 : 1;
            if (ch + count > dec->channels) return ESP_ERR_NOT_SUPPORTED;
            element_t e;
            const esp_err_t err = read_element(d, &b, count, &e);
            if (err != ESP_OK) return err;
            if (total && e.frames != total) return ESP_ERR_INVALID_RESPONSE;
            total = e.frames;
            write_element(d, &e, count, pcm, ch);
            ch += count;
            break;
        }
        case ID_DSE: {
            read_bits(&b, 4);
            const bool align = read_bits(&b, 1);
            uint32_t count = read_bits(&b, 8);
            if (count == 255) count += read_bits(&b, 8);
            if (align) b.pos = (b.pos + 7) & ~(size_t)7;
            b.pos += (size_t)count * 8;
            break;
        }
        case ID_FIL: {
            uint32_t count = read_bits(&b, 4);
            if (count == 15) count += read_bits(&b, 8) - 1;
            b.pos += (size_t)count * 8;
            break;
        }
        case ID_END:
            return ESP_ERR_NOT_SUPPORTED;
        default:
            return ESP_ERR_INVALID_RESPONSE;
        }
    }
    *frames = total;
    return ESP_OK;
}

static void destroy(audf_decoder_t *dec) {
    alac_decoder_t *d = (alac_decoder_t *)dec;
    audf_free(d->predictor);
    audf_free(d);
}

static const audf_decoder_ops_t s_ops = {
    .decode = decode,
    .destroy = destroy,
};

esp_err_t audf_alac_decoder_create(const audf_alac_config_t *config, audf_decoder_t **out) {
    if (!out || !config || !config->cookie || config->cookie_len < AUDF_ALAC_COOKIE_BYTES) {
        return ESP_ERR_INVALID_ARG;
    }
    const uint8_t *c = config->cookie;
    const uint32_t frame_length = be32(c);
    const uint8_t version = c[4], bits = c[5], kb = c[8], channels = c[9];
    const uint32_t rate = be32(c + 20);
    if (version != 0 || !frame_length || frame_length > ALAC_MAX_FRAME || !bits || bits > ALAC_MAX_BITS ||
        kb > 31 || !channels || channels > ALAC_MAX_CHANNELS || !rate) {
        return ESP_ERR_NOT_SUPPORTED;
    }

    alac_decoder_t *d = audf_calloc(1, sizeof(*d), config->alloc_caps);
    if (!d) return ESP_ERR_NO_MEM;
    const size_t buffers = channels > 1 ? 3 : 2;
    d->predictor = audf_malloc(buffers * frame_length * sizeof(int32_t), config->alloc_caps);
    if (!d->predictor) {
        audf_free(d);
        return ESP_ERR_NO_MEM;
    }
    d->mix[0] = d->predictor + frame_length;
    d->mix[1] = channels > 1 ? d->predictor + 2 * frame_length : NULL;
    d->base.ops = &s_ops;
    d->base.fmt = bits <= 16 ? AUDF_FMT_S16 : AUDF_FMT_S32;
    d->base.channels = channels;
    d->base.max_frames = frame_length;
    d->rate = rate;
    d->bits = bits;
    d->pb = c[6];
    d->mb = c[7];
    d->kb = kb;
    *out = &d->base;
    return ESP_OK;
}

uint32_t audf_alac_decoder_rate(const audf_decoder_t *dec) {
    return ((const alac_decoder_t *)dec)->rate;
}
