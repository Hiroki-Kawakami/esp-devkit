/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "audf_aac.h"

typedef struct {
    const uint8_t *data;
    size_t size;
} span_t;

typedef struct {
    uint8_t  asc[64];
    size_t   asc_len;
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

static size_t desc_len(const uint8_t **p, const uint8_t *end) {
    size_t len = 0;
    for (int i = 0; i < 4 && *p < end; i++) {
        const uint8_t b = *(*p)++;
        len = len << 7 | (b & 0x7f);
        if (!(b & 0x80)) break;
    }
    return len;
}

static bool parse_esds(span_t esds, track_t *t) {
    const uint8_t *p = esds.data + 4, *end = esds.data + esds.size;
    if (p >= end || *p++ != 3) return false;
    desc_len(&p, end);
    p += 2;
    const uint8_t flags = *p++;
    if (flags & 0x80) p += 2;
    if (flags & 0x40) p += 1 + *p;
    if (flags & 0x20) p += 2;
    if (p >= end || *p++ != 4) return false;
    desc_len(&p, end);
    p += 13;
    if (p >= end || *p++ != 5) return false;
    const size_t len = desc_len(&p, end);
    if (len > sizeof(t->asc) || p + len > end) return false;
    memcpy(t->asc, p, len);
    t->asc_len = len;
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
                !memcmp(stsd.data + 12, "mp4a", 4)) {
                span_t entry = { stsd.data + 16 + 28, be32(stsd.data + 8) - 8 - 28 };
                span_t esds, stsz, stsc, stco;
                if (!find_box(entry, "esds", &esds) || !parse_esds(esds, t)) return false;
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

static bool parse_adts(span_t file, track_t *t) {
    size_t pos = 0, n = 0;
    while (pos < file.size) {
        const size_t len = audf_aac_adts_frame_len(file.data + pos, file.size - pos);
        if (!len || pos + len > file.size) break;
        pos += len;
        n++;
    }
    if (!n) return false;
    t->count = (uint32_t)n;
    t->offset = calloc(n, sizeof(uint64_t));
    t->size = calloc(n, sizeof(uint32_t));
    pos = 0;
    for (size_t i = 0; i < n; i++) {
        t->offset[i] = pos;
        t->size[i] = (uint32_t)audf_aac_adts_frame_len(file.data + pos, file.size - pos);
        pos += t->size[i];
    }
    return true;
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

static void compare(const int16_t *ours, size_t frames, int channels, const char *ref_path) {
    size_t bytes;
    float *ref = (float *)read_file(ref_path, &bytes);
    if (!ref) {
        fprintf(stderr, "cannot read %s\n", ref_path);
        return;
    }
    const size_t ref_frames = bytes / sizeof(float) / channels;
    const size_t n = frames < ref_frames ? frames : ref_frames;
    int best_lag = 0;
    double best_err = INFINITY;
    for (int lag = -2048; lag <= 2048; lag++) {
        double err = 0;
        for (size_t i = 4096; i < n && i < 4096 + 8192; i++) {
            const long j = (long)i + lag;
            if (j < 0 || (size_t)j >= ref_frames) continue;
            const double d = ours[i * channels] - ref[j * channels] * 32768.0;
            err += d * d;
        }
        if (err < best_err) {
            best_err = err;
            best_lag = lag;
        }
    }
    printf("frames ours %zu ref %zu, best lag %d\n", frames, ref_frames, best_lag);
    for (int ch = 0; ch < channels; ch++) {
        double sig = 0, err = 0, max = 0;
        for (size_t i = 0; i < n; i++) {
            const double r = ref[i * channels + ch] * 32768.0;
            const double d = ours[i * channels + ch] - r;
            sig += r * r;
            err += d * d;
            if (fabs(d) > max) max = fabs(d);
        }
        printf("ch%d snr %.1f dB, max diff %.1f\n", ch, err > 0 ? 10 * log10(sig / err) : 999.0, max);
    }
    free(ref);
}

int main(int argc, char **argv) {
    audf_aac_he_t he = AUDF_AAC_HE_V2;
    const char *in = NULL, *out = NULL, *ref = NULL;
    int loops = 1;
    bool hash = false;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--he") && i + 1 < argc) {
            const char *m = argv[++i];
            he = !strcmp(m, "off") ? AUDF_AAC_HE_OFF : !strcmp(m, "v1") ? AUDF_AAC_HE_V1 : AUDF_AAC_HE_V2;
        } else if (!strcmp(argv[i], "--ref") && i + 1 < argc) {
            ref = argv[++i];
        } else if (!strcmp(argv[i], "--hash")) {
            hash = true;
        } else if (!strcmp(argv[i], "--loops") && i + 1 < argc) {
            loops = atoi(argv[++i]);
        } else if (!in) {
            in = argv[i];
        } else {
            out = argv[i];
        }
    }
    if (!in) {
        fprintf(stderr, "usage: aac_dec_test [--he off|v1|v2] [--ref ref.f32] [--loops n] in.(aac|m4a) [out.s16]\n");
        return 2;
    }
    size_t size;
    uint8_t *file = read_file(in, &size);
    if (!file) {
        fprintf(stderr, "cannot read %s\n", in);
        return 1;
    }
    track_t t = { 0 };
    span_t span = { file, size };
    const bool adts = parse_adts(span, &t);
    if (!adts && !parse_mp4(span, &t)) {
        fprintf(stderr, "no AAC track in %s\n", in);
        return 1;
    }
    audf_aac_config_t config = {
        .asc = adts ? file + t.offset[0] : t.asc,
        .asc_len = adts ? t.size[0] : t.asc_len,
        .adts = adts,
        .he = he,
    };
    audf_decoder_t *dec;
    esp_err_t err = audf_aac_decoder_create(&config, &dec);
    if (err != ESP_OK) {
        fprintf(stderr, "create: %d\n", err);
        return 1;
    }
    const int channels = audf_decoder_channels(dec);
    const size_t max = audf_decoder_max_frames(dec);
    int16_t *pcm = malloc((size_t)t.count * max * channels * sizeof(int16_t));
    size_t total = 0;
    int errors = 0;
    const clock_t start = clock();
    for (int loop = 0; loop < loops; loop++) {
        total = 0;
        if (loop) audf_decoder_reset(dec);
        for (uint32_t i = 0; i < t.count; i++) {
            size_t frames = 0;
            if (t.offset[i] + t.size[i] > size) break;
            err = audf_decoder_decode(dec, file + t.offset[i], t.size[i], pcm + total * channels, &frames);
            if (err != ESP_OK) {
                if (!errors++) fprintf(stderr, "frame %u: error %d\n", i, err);
                continue;
            }
            total += frames;
        }
        if (hash && loops > 1) {
            uint32_t h = 2166136261u;
            const uint8_t *bytes = (const uint8_t *)pcm;
            for (size_t i = 0; i < total * channels * sizeof(int16_t); i++) h = (h ^ bytes[i]) * 16777619u;
            printf("[AACPCM] loop %d h=%08x\n", loop, h);
        }
    }
    const double secs = (double)(clock() - start) / CLOCKS_PER_SEC;
    printf("%u Hz, %d ch, %zu frames, %u packets, %d errors, %.1fx realtime\n", audf_aac_decoder_rate(dec),
           channels, total, t.count, errors, secs > 0 ? loops * (double)total / audf_aac_decoder_rate(dec) / secs : 0);
    if (out) {
        FILE *f = fopen(out, "wb");
        fwrite(pcm, sizeof(int16_t), total * channels, f);
        fclose(f);
    }
    if (hash) {
        uint32_t h = 2166136261u;
        const uint8_t *bytes = (const uint8_t *)pcm;
        for (size_t i = 0; i < total * channels * sizeof(int16_t); i++) h = (h ^ bytes[i]) * 16777619u;
        printf("[AACPCM] h=%08x samples=%zu\n", h, total);
    }
    if (ref) compare(pcm, total, channels, ref);
    audf_decoder_destroy(dec);
    free(pcm);
    free(t.offset);
    free(t.size);
    free(file);
    return errors ? 1 : 0;
}
