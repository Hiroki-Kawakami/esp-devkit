/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "audf_flac.h"

typedef struct {
    uint32_t state[4];
    uint64_t length;
    uint8_t  block[64];
    size_t   used;
} md5_t;

static const uint32_t s_md5_k[64] = {
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
};

static const uint8_t s_md5_r[64] = {
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
};

static void md5_block(md5_t *m) {
    uint32_t w[16];
    for (int i = 0; i < 16; i++) {
        w[i] = (uint32_t)m->block[i * 4] | (uint32_t)m->block[i * 4 + 1] << 8 |
               (uint32_t)m->block[i * 4 + 2] << 16 | (uint32_t)m->block[i * 4 + 3] << 24;
    }
    uint32_t a = m->state[0], b = m->state[1], c = m->state[2], d = m->state[3];
    for (int i = 0; i < 64; i++) {
        uint32_t f;
        int g;
        if (i < 16) {
            f = (b & c) | (~b & d);
            g = i;
        } else if (i < 32) {
            f = (d & b) | (~d & c);
            g = (5 * i + 1) % 16;
        } else if (i < 48) {
            f = b ^ c ^ d;
            g = (3 * i + 5) % 16;
        } else {
            f = c ^ (b | ~d);
            g = (7 * i) % 16;
        }
        const uint32_t t = d;
        d = c;
        c = b;
        const uint32_t x = a + f + s_md5_k[i] + w[g];
        b += x << s_md5_r[i] | x >> (32 - s_md5_r[i]);
        a = t;
    }
    m->state[0] += a;
    m->state[1] += b;
    m->state[2] += c;
    m->state[3] += d;
}

static void md5_init(md5_t *m) {
    *m = (md5_t){ .state = { 0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476 } };
}

static void md5_update(md5_t *m, const uint8_t *p, size_t n) {
    m->length += n;
    while (n--) {
        m->block[m->used++] = *p++;
        if (m->used == 64) {
            md5_block(m);
            m->used = 0;
        }
    }
}

static void md5_final(md5_t *m, uint8_t out[16]) {
    const uint64_t bits = m->length * 8;
    const uint8_t one = 0x80, zero = 0;
    md5_update(m, &one, 1);
    while (m->used != 56) md5_update(m, &zero, 1);
    uint8_t len[8];
    for (int i = 0; i < 8; i++) len[i] = (uint8_t)(bits >> (8 * i));
    md5_update(m, len, 8);
    for (int i = 0; i < 16; i++) out[i] = (uint8_t)(m->state[i / 4] >> (8 * (i % 4)));
}

static uint8_t *read_file(const char *path, size_t *size) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    *size = (size_t)ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *data = malloc(*size ? *size : 1);
    if (data && fread(data, 1, *size, f) != *size) {
        free(data);
        data = NULL;
    }
    fclose(f);
    return data;
}

static bool parse_header(const uint8_t *data, size_t size, const uint8_t **streaminfo, size_t *first) {
    size_t pos = 0;
    if (size >= 10 && !memcmp(data, "ID3", 3)) {
        pos = 10 + ((size_t)(data[6] & 0x7f) << 21 | (size_t)(data[7] & 0x7f) << 14 |
                    (size_t)(data[8] & 0x7f) << 7 | (data[9] & 0x7f));
    }
    if (pos + 4 > size || memcmp(data + pos, "fLaC", 4)) return false;
    pos += 4;
    *streaminfo = NULL;
    for (bool last = false; !last;) {
        if (pos + 4 > size) return false;
        last = data[pos] & 0x80;
        const uint8_t type = data[pos] & 0x7f;
        const size_t len = (size_t)data[pos + 1] << 16 | (size_t)data[pos + 2] << 8 | data[pos + 3];
        pos += 4;
        if (pos + len > size) return false;
        if (type == 0 && len >= AUDF_FLAC_STREAMINFO_BYTES) *streaminfo = data + pos;
        pos += len;
    }
    *first = pos;
    return *streaminfo != NULL;
}

static bool is_sync(const uint8_t *p) {
    return p[0] == 0xff && (p[1] & 0xfe) == 0xf8;
}

/* A frame has no length, so it ends at the first sync word after which it
 * decodes. */
static esp_err_t decode_next(audf_decoder_t *dec, const uint8_t *data, size_t size, size_t pos,
                             void *pcm, size_t *frames, size_t *next) {
    for (size_t end = pos + 2; end + 2 <= size; end++) {
        if (!is_sync(data + end)) continue;
        if (audf_decoder_decode(dec, data + pos, end - pos, pcm, frames) == ESP_OK) {
            *next = end;
            return ESP_OK;
        }
    }
    *next = size;
    return audf_decoder_decode(dec, data + pos, size - pos, pcm, frames);
}

int main(int argc, char **argv) {
    const char *in = NULL, *out = NULL;
    int loops = 1;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--loops") && i + 1 < argc) {
            loops = atoi(argv[++i]);
        } else if (!in) {
            in = argv[i];
        } else {
            out = argv[i];
        }
    }
    if (!in) {
        fprintf(stderr, "usage: flac_dec_test [--loops n] in.flac [out.raw]\n");
        return 2;
    }
    size_t size;
    uint8_t *file = read_file(in, &size);
    if (!file) {
        fprintf(stderr, "cannot read %s\n", in);
        return 1;
    }
    const uint8_t *si;
    size_t first;
    if (!parse_header(file, size, &si, &first)) {
        fprintf(stderr, "no FLAC stream in %s\n", in);
        return 1;
    }
    const audf_flac_config_t config = { .streaminfo = si, .streaminfo_len = AUDF_FLAC_STREAMINFO_BYTES };
    audf_decoder_t *dec;
    esp_err_t err = audf_flac_decoder_create(&config, &dec);
    if (err != ESP_OK) {
        fprintf(stderr, "create: %d\n", err);
        return 1;
    }
    const int bps = ((si[12] & 1) << 4 | si[13] >> 4) + 1;
    const int bytes = (bps + 7) / 8;
    const int shift = (audf_decoder_fmt(dec) == AUDF_FMT_S16 ? 16 : 24) - bps;
    const int channels = audf_decoder_channels(dec);
    const size_t max = audf_decoder_max_frames(dec);
    void *pcm = malloc(max * channels * sizeof(int32_t));
    uint8_t *raw = malloc(max * channels * 4);
    FILE *f = out ? fopen(out, "wb") : NULL;

    md5_t md5;
    size_t total = 0, packets = 0;
    int errors = 0;
    const clock_t start = clock();
    for (int loop = 0; loop < loops; loop++) {
        md5_init(&md5);
        total = packets = 0;
        size_t pos = first;
        while (pos + 2 <= size) {
            if (!is_sync(file + pos)) {
                pos++;
                continue;
            }
            size_t frames = 0, next;
            err = decode_next(dec, file, size, pos, pcm, &frames, &next);
            if (err != ESP_OK) {
                if (!errors++) fprintf(stderr, "frame at %zu: error %d\n", pos, err);
                pos++;
                continue;
            }
            pos = next;
            packets++;
            size_t n = 0;
            for (size_t i = 0; i < frames * channels; i++) {
                const int32_t v = audf_decoder_fmt(dec) == AUDF_FMT_S16 ? ((const int16_t *)pcm)[i]
                                                                         : ((const int32_t *)pcm)[i];
                const int32_t s = v >> shift;
                for (int b = 0; b < bytes; b++) raw[n++] = (uint8_t)((uint32_t)s >> (8 * b));
            }
            md5_update(&md5, raw, n);
            if (f && !loop) fwrite(raw, 1, n, f);
            total += frames;
        }
    }
    const double secs = (double)(clock() - start) / CLOCKS_PER_SEC;
    if (f) fclose(f);

    uint8_t sum[16];
    md5_final(&md5, sum);
    static const uint8_t unset[16];
    const bool has_md5 = memcmp(si + 18, unset, 16) != 0;
    const bool md5_ok = !memcmp(si + 18, sum, 16);
    printf("%u Hz, %d ch, %d bit, %zu frames, %zu packets, %d errors, %.1fx realtime, md5 %s\n",
           audf_flac_decoder_rate(dec), channels, bps, total, packets, errors,
           secs > 0 ? loops * (double)total / audf_flac_decoder_rate(dec) / secs : 0,
           !has_md5 ? "unset" : md5_ok ? "ok" : "MISMATCH");
    audf_decoder_destroy(dec);
    free(pcm);
    free(raw);
    free(file);
    return errors || (has_md5 && !md5_ok) ? 1 : 0;
}
