/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_flac.h"
#include <stdbool.h>
#include "audf_alloc.h"
#include "audf_codec_internal.h"

#define FLAC_MAX_BPS 24
#define FLAC_MAX_ORDER 32

static const uint8_t s_crc8[256] = {
    0x00, 0x07, 0x0e, 0x09, 0x1c, 0x1b, 0x12, 0x15, 0x38, 0x3f, 0x36, 0x31,
    0x24, 0x23, 0x2a, 0x2d, 0x70, 0x77, 0x7e, 0x79, 0x6c, 0x6b, 0x62, 0x65,
    0x48, 0x4f, 0x46, 0x41, 0x54, 0x53, 0x5a, 0x5d, 0xe0, 0xe7, 0xee, 0xe9,
    0xfc, 0xfb, 0xf2, 0xf5, 0xd8, 0xdf, 0xd6, 0xd1, 0xc4, 0xc3, 0xca, 0xcd,
    0x90, 0x97, 0x9e, 0x99, 0x8c, 0x8b, 0x82, 0x85, 0xa8, 0xaf, 0xa6, 0xa1,
    0xb4, 0xb3, 0xba, 0xbd, 0xc7, 0xc0, 0xc9, 0xce, 0xdb, 0xdc, 0xd5, 0xd2,
    0xff, 0xf8, 0xf1, 0xf6, 0xe3, 0xe4, 0xed, 0xea, 0xb7, 0xb0, 0xb9, 0xbe,
    0xab, 0xac, 0xa5, 0xa2, 0x8f, 0x88, 0x81, 0x86, 0x93, 0x94, 0x9d, 0x9a,
    0x27, 0x20, 0x29, 0x2e, 0x3b, 0x3c, 0x35, 0x32, 0x1f, 0x18, 0x11, 0x16,
    0x03, 0x04, 0x0d, 0x0a, 0x57, 0x50, 0x59, 0x5e, 0x4b, 0x4c, 0x45, 0x42,
    0x6f, 0x68, 0x61, 0x66, 0x73, 0x74, 0x7d, 0x7a, 0x89, 0x8e, 0x87, 0x80,
    0x95, 0x92, 0x9b, 0x9c, 0xb1, 0xb6, 0xbf, 0xb8, 0xad, 0xaa, 0xa3, 0xa4,
    0xf9, 0xfe, 0xf7, 0xf0, 0xe5, 0xe2, 0xeb, 0xec, 0xc1, 0xc6, 0xcf, 0xc8,
    0xdd, 0xda, 0xd3, 0xd4, 0x69, 0x6e, 0x67, 0x60, 0x75, 0x72, 0x7b, 0x7c,
    0x51, 0x56, 0x5f, 0x58, 0x4d, 0x4a, 0x43, 0x44, 0x19, 0x1e, 0x17, 0x10,
    0x05, 0x02, 0x0b, 0x0c, 0x21, 0x26, 0x2f, 0x28, 0x3d, 0x3a, 0x33, 0x34,
    0x4e, 0x49, 0x40, 0x47, 0x52, 0x55, 0x5c, 0x5b, 0x76, 0x71, 0x78, 0x7f,
    0x6a, 0x6d, 0x64, 0x63, 0x3e, 0x39, 0x30, 0x37, 0x22, 0x25, 0x2c, 0x2b,
    0x06, 0x01, 0x08, 0x0f, 0x1a, 0x1d, 0x14, 0x13, 0xae, 0xa9, 0xa0, 0xa7,
    0xb2, 0xb5, 0xbc, 0xbb, 0x96, 0x91, 0x98, 0x9f, 0x8a, 0x8d, 0x84, 0x83,
    0xde, 0xd9, 0xd0, 0xd7, 0xc2, 0xc5, 0xcc, 0xcb, 0xe6, 0xe1, 0xe8, 0xef,
    0xfa, 0xfd, 0xf4, 0xf3,
};

static const uint16_t s_crc16[256] = {
    0x0000, 0x8005, 0x800f, 0x000a, 0x801b, 0x001e, 0x0014, 0x8011, 0x8033, 0x0036,
    0x003c, 0x8039, 0x0028, 0x802d, 0x8027, 0x0022, 0x8063, 0x0066, 0x006c, 0x8069,
    0x0078, 0x807d, 0x8077, 0x0072, 0x0050, 0x8055, 0x805f, 0x005a, 0x804b, 0x004e,
    0x0044, 0x8041, 0x80c3, 0x00c6, 0x00cc, 0x80c9, 0x00d8, 0x80dd, 0x80d7, 0x00d2,
    0x00f0, 0x80f5, 0x80ff, 0x00fa, 0x80eb, 0x00ee, 0x00e4, 0x80e1, 0x00a0, 0x80a5,
    0x80af, 0x00aa, 0x80bb, 0x00be, 0x00b4, 0x80b1, 0x8093, 0x0096, 0x009c, 0x8099,
    0x0088, 0x808d, 0x8087, 0x0082, 0x8183, 0x0186, 0x018c, 0x8189, 0x0198, 0x819d,
    0x8197, 0x0192, 0x01b0, 0x81b5, 0x81bf, 0x01ba, 0x81ab, 0x01ae, 0x01a4, 0x81a1,
    0x01e0, 0x81e5, 0x81ef, 0x01ea, 0x81fb, 0x01fe, 0x01f4, 0x81f1, 0x81d3, 0x01d6,
    0x01dc, 0x81d9, 0x01c8, 0x81cd, 0x81c7, 0x01c2, 0x0140, 0x8145, 0x814f, 0x014a,
    0x815b, 0x015e, 0x0154, 0x8151, 0x8173, 0x0176, 0x017c, 0x8179, 0x0168, 0x816d,
    0x8167, 0x0162, 0x8123, 0x0126, 0x012c, 0x8129, 0x0138, 0x813d, 0x8137, 0x0132,
    0x0110, 0x8115, 0x811f, 0x011a, 0x810b, 0x010e, 0x0104, 0x8101, 0x8303, 0x0306,
    0x030c, 0x8309, 0x0318, 0x831d, 0x8317, 0x0312, 0x0330, 0x8335, 0x833f, 0x033a,
    0x832b, 0x032e, 0x0324, 0x8321, 0x0360, 0x8365, 0x836f, 0x036a, 0x837b, 0x037e,
    0x0374, 0x8371, 0x8353, 0x0356, 0x035c, 0x8359, 0x0348, 0x834d, 0x8347, 0x0342,
    0x03c0, 0x83c5, 0x83cf, 0x03ca, 0x83db, 0x03de, 0x03d4, 0x83d1, 0x83f3, 0x03f6,
    0x03fc, 0x83f9, 0x03e8, 0x83ed, 0x83e7, 0x03e2, 0x83a3, 0x03a6, 0x03ac, 0x83a9,
    0x03b8, 0x83bd, 0x83b7, 0x03b2, 0x0390, 0x8395, 0x839f, 0x039a, 0x838b, 0x038e,
    0x0384, 0x8381, 0x0280, 0x8285, 0x828f, 0x028a, 0x829b, 0x029e, 0x0294, 0x8291,
    0x82b3, 0x02b6, 0x02bc, 0x82b9, 0x02a8, 0x82ad, 0x82a7, 0x02a2, 0x82e3, 0x02e6,
    0x02ec, 0x82e9, 0x02f8, 0x82fd, 0x82f7, 0x02f2, 0x02d0, 0x82d5, 0x82df, 0x02da,
    0x82cb, 0x02ce, 0x02c4, 0x82c1, 0x8243, 0x0246, 0x024c, 0x8249, 0x0258, 0x825d,
    0x8257, 0x0252, 0x0270, 0x8275, 0x827f, 0x027a, 0x826b, 0x026e, 0x0264, 0x8261,
    0x0220, 0x8225, 0x822f, 0x022a, 0x823b, 0x023e, 0x0234, 0x8231, 0x8213, 0x0216,
    0x021c, 0x8219, 0x0208, 0x820d, 0x8207, 0x0202,
};

typedef struct {
    audf_decoder_t base;
    uint32_t rate;
    uint8_t  bps;
    int32_t *samples;
} flac_decoder_t;

typedef struct {
    const uint8_t *p;
    const uint8_t *end;
    uint64_t cache;
    int      bits;
    size_t   pad;
} bits_t;

static void refill(bits_t *b) {
    while (b->bits <= 56) {
        uint8_t byte = 0;
        if (b->p < b->end) {
            byte = *b->p++;
        } else {
            b->pad++;
        }
        b->cache |= (uint64_t)byte << (56 - b->bits);
        b->bits += 8;
    }
}

static uint32_t read_bits(bits_t *b, int n) {
    if (!n) return 0;
    if (b->bits < n) refill(b);
    const uint32_t v = (uint32_t)(b->cache >> (64 - n));
    b->cache <<= n;
    b->bits -= n;
    return v;
}

static int32_t read_signed(bits_t *b, int n) {
    if (!n) return 0;
    const uint32_t v = read_bits(b, n) << (32 - n);
    return (int32_t)v >> (32 - n);
}

static bool overrun(const bits_t *b) {
    return b->pad * 8 > (size_t)b->bits;
}

static uint32_t read_unary(bits_t *b) {
    uint32_t zeros = 0;
    for (;;) {
        if (b->bits < 64) refill(b);
        if (b->cache) {
            const int z = __builtin_clzll(b->cache);
            b->cache = (b->cache << z) << 1;
            b->bits -= z + 1;
            return zeros + (uint32_t)z;
        }
        zeros += (uint32_t)b->bits;
        b->cache = 0;
        b->bits = 0;
        if (overrun(b)) return zeros;
    }
}

static size_t consumed(const bits_t *b, const uint8_t *start) {
    return (size_t)(b->p - start) + b->pad - (size_t)b->bits / 8;
}

static uint8_t crc8(const uint8_t *p, size_t n) {
    uint8_t crc = 0;
    while (n--) crc = s_crc8[crc ^ *p++];
    return crc;
}

static uint16_t crc16(const uint8_t *p, size_t n) {
    uint16_t crc = 0;
    while (n--) crc = (uint16_t)(crc << 8) ^ s_crc16[(crc >> 8) ^ *p++];
    return crc;
}

static const uint32_t s_rates[12] = {
    0, 88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000,
};

static const uint8_t s_sizes[8] = { 0, 8, 12, 0, 16, 20, 24, 32 };

typedef struct {
    uint32_t block;
    uint8_t  assignment;
} frame_header_t;

static esp_err_t read_header(flac_decoder_t *d, bits_t *b, const uint8_t *frame, frame_header_t *h) {
    if (read_bits(b, 14) != 0x3FFE || read_bits(b, 1)) return ESP_ERR_INVALID_RESPONSE;
    read_bits(b, 1);
    const uint32_t block_code = read_bits(b, 4);
    const uint32_t rate_code = read_bits(b, 4);
    const uint32_t assignment = read_bits(b, 4);
    const uint32_t size_code = read_bits(b, 3);
    if (read_bits(b, 1)) return ESP_ERR_INVALID_RESPONSE;

    const uint32_t lead = read_bits(b, 8);
    int extra = 0;
    while (extra < 8 && (lead & (0x80u >> extra))) extra++;
    if (extra == 1 || extra == 8) return ESP_ERR_INVALID_RESPONSE;
    for (int i = 1; i < extra; i++) {
        if (read_bits(b, 8) >> 6 != 2) return ESP_ERR_INVALID_RESPONSE;
    }

    if (block_code == 0) return ESP_ERR_INVALID_RESPONSE;
    if (block_code == 1) {
        h->block = 192;
    } else if (block_code <= 5) {
        h->block = 576u << (block_code - 2);
    } else if (block_code == 6) {
        h->block = read_bits(b, 8) + 1;
    } else if (block_code == 7) {
        h->block = read_bits(b, 16) + 1;
    } else {
        h->block = 256u << (block_code - 8);
    }

    uint32_t rate = d->rate;
    if (rate_code == 15) return ESP_ERR_INVALID_RESPONSE;
    if (rate_code >= 1 && rate_code <= 11) rate = s_rates[rate_code];
    if (rate_code == 12) rate = read_bits(b, 8) * 1000;
    if (rate_code == 13) rate = read_bits(b, 16);
    if (rate_code == 14) rate = read_bits(b, 16) * 10;

    const size_t len = consumed(b, frame);
    const uint32_t crc = read_bits(b, 8);
    if (overrun(b)) return ESP_ERR_INVALID_SIZE;
    if (crc != crc8(frame, len)) return ESP_ERR_INVALID_CRC;

    const uint8_t channels = assignment < 8 ? (uint8_t)(assignment + 1) : 2;
    const uint8_t bps = size_code ? s_sizes[size_code] : d->bps;
    if (assignment > 10 || channels != d->base.channels || rate != d->rate || bps != d->bps) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (h->block > d->base.max_frames) return ESP_ERR_INVALID_SIZE;
    h->assignment = (uint8_t)assignment;
    return ESP_OK;
}

static esp_err_t read_residual(bits_t *b, int32_t *s, uint32_t block, uint32_t order) {
    const uint32_t method = read_bits(b, 2);
    if (method > 1) return ESP_ERR_INVALID_RESPONSE;
    const int param_bits = method ? 5 : 4;
    const uint32_t escape = method ? 31 : 15;
    const uint32_t partition_order = read_bits(b, 4);
    const uint32_t partitions = 1u << partition_order;
    const uint32_t per = block >> partition_order;
    if (per << partition_order != block || per < order) return ESP_ERR_INVALID_RESPONSE;

    int32_t *out = s + order;
    for (uint32_t part = 0; part < partitions; part++) {
        const uint32_t n = part ? per : per - order;
        const uint32_t k = read_bits(b, param_bits);
        if (k == escape) {
            const int raw = (int)read_bits(b, 5);
            for (uint32_t i = 0; i < n; i++) out[i] = read_signed(b, raw);
        } else {
            for (uint32_t i = 0; i < n; i++) {
                const uint32_t v = read_unary(b) << k | read_bits(b, (int)k);
                out[i] = (int32_t)(v >> 1) ^ -(int32_t)(v & 1);
            }
        }
        if (overrun(b)) return ESP_ERR_INVALID_SIZE;
        out += n;
    }
    return ESP_OK;
}

static void restore_fixed(int32_t *s, uint32_t block, uint32_t order) {
    uint32_t *u = (uint32_t *)s;
    switch (order) {
    case 1:
        for (uint32_t i = 1; i < block; i++) u[i] += u[i - 1];
        break;
    case 2:
        for (uint32_t i = 2; i < block; i++) u[i] += 2 * u[i - 1] - u[i - 2];
        break;
    case 3:
        for (uint32_t i = 3; i < block; i++) u[i] += 3 * (u[i - 1] - u[i - 2]) + u[i - 3];
        break;
    case 4:
        for (uint32_t i = 4; i < block; i++) u[i] += 4 * (u[i - 1] + u[i - 3]) - 6 * u[i - 2] - u[i - 4];
        break;
    default:
        break;
    }
}

static void restore_lpc(int32_t *s, uint32_t block, const int32_t *coef, uint32_t order, int shift,
                        bool wide) {
    if (wide) {
        for (uint32_t i = order; i < block; i++) {
            int64_t sum = 0;
            for (uint32_t j = 0; j < order; j++) sum += (int64_t)coef[j] * s[i - 1 - j];
            s[i] = (int32_t)((uint32_t)s[i] + (uint32_t)(int32_t)(sum >> shift));
        }
        return;
    }
    const uint32_t *c = (const uint32_t *)coef;
    for (uint32_t i = order; i < block; i++) {
        const uint32_t *x = (const uint32_t *)s + i - 1;
        uint32_t sum = 0;
        for (uint32_t j = 0; j < order; j++) sum += c[j] * x[-(int32_t)j];
        s[i] = (int32_t)((uint32_t)s[i] + (uint32_t)((int32_t)sum >> shift));
    }
}

static uint32_t bit_length(uint32_t v) {
    return v ? 32u - (uint32_t)__builtin_clz(v) : 0;
}

static esp_err_t read_subframe(bits_t *b, int32_t *s, uint32_t block, int bps) {
    if (read_bits(b, 1)) return ESP_ERR_INVALID_RESPONSE;
    const uint32_t type = read_bits(b, 6);
    int wasted = 0;
    if (read_bits(b, 1)) wasted = (int)read_unary(b) + 1;
    bps -= wasted;
    if (bps <= 0) return ESP_ERR_INVALID_RESPONSE;

    esp_err_t err = ESP_OK;
    if (type == 0) {
        const int32_t v = read_signed(b, bps);
        for (uint32_t i = 0; i < block; i++) s[i] = v;
    } else if (type == 1) {
        for (uint32_t i = 0; i < block; i++) s[i] = read_signed(b, bps);
    } else if (type >= 8 && type <= 12) {
        const uint32_t order = type - 8;
        if (order > block) return ESP_ERR_INVALID_RESPONSE;
        for (uint32_t i = 0; i < order; i++) s[i] = read_signed(b, bps);
        err = read_residual(b, s, block, order);
        if (err == ESP_OK) restore_fixed(s, block, order);
    } else if (type >= 32) {
        const uint32_t order = type - 31;
        if (order > block) return ESP_ERR_INVALID_RESPONSE;
        for (uint32_t i = 0; i < order; i++) s[i] = read_signed(b, bps);
        const uint32_t precision = read_bits(b, 4) + 1;
        const int shift = read_signed(b, 5);
        if (precision == 16 || shift < 0) return ESP_ERR_INVALID_RESPONSE;
        int32_t coef[FLAC_MAX_ORDER];
        for (uint32_t i = 0; i < order; i++) coef[i] = read_signed(b, (int)precision);
        err = read_residual(b, s, block, order);
        const bool wide = (uint32_t)bps + precision + bit_length(order) > 32;
        if (err == ESP_OK) restore_lpc(s, block, coef, order, shift, wide);
    } else {
        return ESP_ERR_INVALID_RESPONSE;
    }
    if (err != ESP_OK) return err;
    if (overrun(b)) return ESP_ERR_INVALID_SIZE;

    if (wasted) {
        for (uint32_t i = 0; i < block; i++) s[i] = (int32_t)((uint32_t)s[i] << wasted);
    }
    return ESP_OK;
}

static void decorrelate(int32_t *a, int32_t *b, uint32_t block, uint8_t assignment) {
    for (uint32_t i = 0; i < block; i++) {
        const uint32_t x = (uint32_t)a[i], y = (uint32_t)b[i];
        switch (assignment) {
        case 8: b[i] = (int32_t)(x - y); break;
        case 9: a[i] = (int32_t)(x + y); break;
        case 10: {
            const uint32_t mid = x << 1 | (y & 1);
            a[i] = (int32_t)(mid + y) >> 1;
            b[i] = (int32_t)(mid - y) >> 1;
            break;
        }
        default: return;
        }
    }
}

static esp_err_t decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    flac_decoder_t *d = (flac_decoder_t *)dec;
    bits_t b = { .p = frame, .end = (const uint8_t *)frame + len };
    frame_header_t h;
    esp_err_t err = read_header(d, &b, frame, &h);
    if (err != ESP_OK) return err;

    const uint8_t channels = dec->channels;
    const size_t stride = dec->max_frames;
    for (uint8_t ch = 0; ch < channels; ch++) {
        const bool side = (h.assignment == 8 && ch == 1) || (h.assignment == 9 && ch == 0) ||
                          (h.assignment == 10 && ch == 1);
        err = read_subframe(&b, d->samples + ch * stride, h.block, d->bps + side);
        if (err != ESP_OK) return err;
    }

    read_bits(&b, b.bits % 8);
    const size_t body = consumed(&b, frame);
    const uint32_t crc = read_bits(&b, 16);
    if (overrun(&b)) return ESP_ERR_INVALID_SIZE;
    if (crc != crc16(frame, body)) return ESP_ERR_INVALID_CRC;

    if (channels == 2) decorrelate(d->samples, d->samples + stride, h.block, h.assignment);
    if (dec->fmt == AUDF_FMT_S16) {
        int16_t *out = pcm;
        const int shift = 16 - d->bps;
        for (uint8_t ch = 0; ch < channels; ch++) {
            const int32_t *s = d->samples + ch * stride;
            for (uint32_t i = 0; i < h.block; i++) {
                out[i * channels + ch] = (int16_t)((uint32_t)s[i] << shift);
            }
        }
    } else {
        int32_t *out = pcm;
        const int shift = 24 - d->bps;
        for (uint8_t ch = 0; ch < channels; ch++) {
            const int32_t *s = d->samples + ch * stride;
            for (uint32_t i = 0; i < h.block; i++) {
                out[i * channels + ch] = (int32_t)((uint32_t)s[i] << shift);
            }
        }
    }
    *frames = h.block;
    return ESP_OK;
}

static void destroy(audf_decoder_t *dec) {
    flac_decoder_t *d = (flac_decoder_t *)dec;
    audf_free(d->samples);
    audf_free(d);
}

static const audf_decoder_ops_t s_ops = {
    .decode = decode,
    .destroy = destroy,
};

esp_err_t audf_flac_decoder_create(const audf_flac_config_t *config, audf_decoder_t **out) {
    if (!out || !config || !config->streaminfo ||
        config->streaminfo_len < AUDF_FLAC_STREAMINFO_BYTES) {
        return ESP_ERR_INVALID_ARG;
    }
    const uint8_t *si = config->streaminfo;
    const uint32_t max_block = (uint32_t)si[2] << 8 | si[3];
    const uint32_t rate = (uint32_t)si[10] << 12 | (uint32_t)si[11] << 4 | si[12] >> 4;
    const uint8_t channels = (uint8_t)(((si[12] >> 1) & 7) + 1);
    const uint8_t bps = (uint8_t)((((si[12] & 1) << 4) | si[13] >> 4) + 1);
    if (!max_block || !rate || bps < 4 || bps > FLAC_MAX_BPS) return ESP_ERR_NOT_SUPPORTED;

    flac_decoder_t *d = audf_calloc(1, sizeof(*d), config->alloc_caps);
    if (!d) return ESP_ERR_NO_MEM;
    d->samples = audf_malloc((size_t)max_block * channels * sizeof(int32_t), config->alloc_caps);
    if (!d->samples) {
        audf_free(d);
        return ESP_ERR_NO_MEM;
    }
    d->base.ops = &s_ops;
    d->base.fmt = bps <= 16 ? AUDF_FMT_S16 : AUDF_FMT_S32;
    d->base.channels = channels;
    d->base.max_frames = max_block;
    d->rate = rate;
    d->bps = bps;
    *out = &d->base;
    return ESP_OK;
}

uint32_t audf_flac_decoder_rate(const audf_decoder_t *dec) {
    return ((const flac_decoder_t *)dec)->rate;
}
