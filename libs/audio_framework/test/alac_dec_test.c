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

#include "audf_alac.h"

typedef struct {
    const uint8_t *data;
    size_t size;
} span_t;

typedef struct {
    uint8_t  cookie[AUDF_ALAC_COOKIE_BYTES];
    uint32_t count;
    uint64_t *offset;
    uint32_t *size;
} track_t;

static uint32_t be32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}

static uint64_t be64(const uint8_t *p) {
    return (uint64_t)be32(p) << 32 | be32(p + 4);
}

static bool find_box(span_t in, const char *type, span_t *out) {
    size_t pos = 0;
    while (pos + 8 <= in.size) {
        uint64_t size = be32(in.data + pos);
        size_t header = 8;
        if (size == 1 && pos + 16 <= in.size) {
            size = be64(in.data + pos + 8);
            header = 16;
        } else if (size == 0) {
            size = in.size - pos;
        }
        if (size < header || pos + size > in.size) return false;
        if (!memcmp(in.data + pos + 4, type, 4)) {
            out->data = in.data + pos + header;
            out->size = size - header;
            return true;
        }
        pos += size;
    }
    return false;
}

static bool find_path(span_t in, const char *const *path, span_t *out) {
    for (; *path; path++) {
        if (!find_box(in, *path, &in)) return false;
    }
    *out = in;
    return true;
}

static bool parse_mp4(span_t file, track_t *t) {
    span_t moov;
    if (!find_box(file, "moov", &moov)) return false;
    size_t pos = 0;
    while (pos + 8 <= moov.size) {
        const uint32_t size = be32(moov.data + pos);
        if (size < 8 || pos + size > moov.size) return false;
        if (!memcmp(moov.data + pos + 4, "trak", 4)) {
            span_t trak = { moov.data + pos + 8, size - 8 };
            span_t stbl, stsd;
            static const char *const path[] = { "mdia", "minf", "stbl", NULL };
            if (find_path(trak, path, &stbl) && find_box(stbl, "stsd", &stsd) && stsd.size > 16 &&
                !memcmp(stsd.data + 12, "alac", 4)) {
                span_t entry = { stsd.data + 16 + 28, be32(stsd.data + 8) - 8 - 28 };
                span_t alac, stsz, stsc, stco;
                if (!find_box(entry, "alac", &alac) || alac.size < 4 + AUDF_ALAC_COOKIE_BYTES) return false;
                memcpy(t->cookie, alac.data + 4, AUDF_ALAC_COOKIE_BYTES);
                if (!find_box(stbl, "stsz", &stsz) || !find_box(stbl, "stsc", &stsc)) return false;
                const bool co64 = !find_box(stbl, "stco", &stco);
                if (co64 && !find_box(stbl, "co64", &stco)) return false;
                const uint32_t fixed = be32(stsz.data + 4);
                t->count = be32(stsz.data + 8);
                t->offset = calloc(t->count, sizeof(uint64_t));
                t->size = calloc(t->count, sizeof(uint32_t));
                for (uint32_t i = 0; i < t->count; i++) t->size[i] = fixed ? fixed : be32(stsz.data + 12 + 4 * i);
                const uint32_t chunks = be32(stco.data + 4);
                const uint32_t entries = be32(stsc.data + 4);
                uint32_t sample = 0;
                for (uint32_t c = 0; c < chunks && sample < t->count; c++) {
                    uint32_t per = 0;
                    for (uint32_t e = 0; e < entries; e++) {
                        if (be32(stsc.data + 8 + 12 * e) <= c + 1) per = be32(stsc.data + 12 + 12 * e);
                    }
                    uint64_t off = co64 ? be64(stco.data + 8 + 8 * c) : be32(stco.data + 8 + 4 * c);
                    for (uint32_t s = 0; s < per && sample < t->count; s++, sample++) {
                        t->offset[sample] = off;
                        off += t->size[sample];
                    }
                }
                return true;
            }
        }
        pos += size;
    }
    return false;
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

int main(int argc, char **argv) {
    const char *in = NULL, *out = NULL, *ref = NULL;
    int loops = 1;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--loops") && i + 1 < argc) {
            loops = atoi(argv[++i]);
        } else if (!strcmp(argv[i], "--ref") && i + 1 < argc) {
            ref = argv[++i];
        } else if (!in) {
            in = argv[i];
        } else {
            out = argv[i];
        }
    }
    if (!in) {
        fprintf(stderr, "usage: alac_dec_test [--loops n] [--ref ref.raw] in.m4a [out.raw]\n");
        return 2;
    }
    size_t size;
    uint8_t *file = read_file(in, &size);
    if (!file) {
        fprintf(stderr, "cannot read %s\n", in);
        return 1;
    }
    track_t t = { 0 };
    if (!parse_mp4((span_t){ file, size }, &t)) {
        fprintf(stderr, "no ALAC track in %s\n", in);
        return 1;
    }
    const audf_alac_config_t config = { .cookie = t.cookie, .cookie_len = sizeof(t.cookie) };
    audf_decoder_t *dec;
    esp_err_t err = audf_alac_decoder_create(&config, &dec);
    if (err != ESP_OK) {
        fprintf(stderr, "create: %d\n", err);
        return 1;
    }
    const bool s16 = audf_decoder_fmt(dec) == AUDF_FMT_S16;
    const int bytes = s16 ? 2 : 3;
    const int channels = audf_decoder_channels(dec);
    const size_t max = audf_decoder_max_frames(dec);
    void *pcm = malloc(max * channels * sizeof(int32_t));
    uint8_t *raw = malloc((size_t)t.count * max * channels * bytes);

    size_t total = 0;
    int errors = 0;
    const clock_t start = clock();
    for (int loop = 0; loop < loops; loop++) {
        total = 0;
        for (uint32_t i = 0; i < t.count; i++) {
            size_t frames = 0;
            if (t.offset[i] + t.size[i] > size) break;
            err = audf_decoder_decode(dec, file + t.offset[i], t.size[i], pcm, &frames);
            if (err != ESP_OK) {
                if (!errors++) fprintf(stderr, "packet %u: error %d\n", i, err);
                continue;
            }
            uint8_t *p = raw + total * channels * bytes;
            for (size_t s = 0; s < frames * channels; s++) {
                const int32_t v = s16 ? ((const int16_t *)pcm)[s] : ((const int32_t *)pcm)[s];
                for (int b = 0; b < bytes; b++) *p++ = (uint8_t)((uint32_t)v >> (8 * b));
            }
            total += frames;
        }
    }
    const double secs = (double)(clock() - start) / CLOCKS_PER_SEC;
    const size_t len = total * channels * bytes;
    if (out) {
        FILE *f = fopen(out, "wb");
        fwrite(raw, 1, len, f);
        fclose(f);
    }

    const char *verdict = "unchecked";
    if (ref) {
        size_t ref_len;
        uint8_t *expect = read_file(ref, &ref_len);
        verdict = expect && ref_len == len && !memcmp(expect, raw, len) ? "ok" : "MISMATCH";
        if (expect && ref_len != len) fprintf(stderr, "length %zu, ref %zu\n", len, ref_len);
        free(expect);
    }
    printf("%u Hz, %d ch, %u bit, %zu frames, %u packets of %zu, %d errors, %.1fx realtime, ref %s\n",
           audf_alac_decoder_rate(dec), channels, t.cookie[5], total, t.count, max, errors,
           secs > 0 ? loops * (double)total / audf_alac_decoder_rate(dec) / secs : 0, verdict);
    audf_decoder_destroy(dec);
    free(pcm);
    free(raw);
    free(t.offset);
    free(t.size);
    free(file);
    return errors || !strcmp(verdict, "MISMATCH") ? 1 : 0;
}
