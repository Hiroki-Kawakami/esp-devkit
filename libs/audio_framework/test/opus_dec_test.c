/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "audf_opus.h"

typedef struct {
    const uint8_t *data;
    size_t size;
} packet_t;

typedef struct {
    packet_t *packets;
    uint32_t *ranges;
    size_t count;
    size_t cap;
    uint8_t head[AUDF_OPUS_HEAD_BYTES];
    int64_t last_granule;
    bool ogg;
} stream_t;

static uint8_t *read_file(const char *path, size_t *size) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    const long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc((size_t)n + 1);
    if (buf && fread(buf, 1, (size_t)n, f) != (size_t)n) {
        free(buf);
        buf = NULL;
    }
    fclose(f);
    *size = (size_t)n;
    return buf;
}

static void push(stream_t *s, const uint8_t *data, size_t size, uint32_t range) {
    if (s->count == s->cap) {
        s->cap = s->cap ? s->cap * 2 : 1024;
        s->packets = realloc(s->packets, s->cap * sizeof(*s->packets));
        s->ranges = realloc(s->ranges, s->cap * sizeof(*s->ranges));
    }
    s->packets[s->count] = (packet_t){ data, size };
    s->ranges[s->count] = range;
    s->count++;
}

/* opus_demo's framing: 32-bit big-endian length and final range, then the packet. */
static bool parse_bit(stream_t *s, const uint8_t *buf, size_t size, int channels) {
    size_t p = 0;
    while (p + 8 <= size) {
        const uint32_t len = (uint32_t)buf[p] << 24 | buf[p + 1] << 16 | buf[p + 2] << 8 | buf[p + 3];
        const uint32_t rng = (uint32_t)buf[p + 4] << 24 | buf[p + 5] << 16 | buf[p + 6] << 8 | buf[p + 7];
        p += 8;
        if (p + len > size) return false;
        push(s, buf + p, len, rng);
        p += len;
    }
    memcpy(s->head, "OpusHead", 8);
    s->head[8] = 1;
    s->head[9] = (uint8_t)channels;
    return true;
}

/* Packets of the first logical stream of an Ogg file, joined across pages
 * into a separate buffer. */
static bool parse_ogg(stream_t *s, const uint8_t *buf, size_t size, uint8_t **joined) {
    uint8_t *out = malloc(size);
    size_t used = 0;
    size_t start = 0;
    size_t p = 0;
    int index = 0;
    *joined = out;
    while (p + 27 <= size && memcmp(buf + p, "OggS", 4) == 0) {
        const int segments = buf[p + 26];
        int64_t granule = 0;
        for (int i = 7; i >= 0; i--) granule = granule << 8 | buf[p + 6 + i];
        const uint8_t *lacing = buf + p + 27;
        size_t body = p + 27 + (size_t)segments;
        for (int i = 0; i < segments; i++) {
            memcpy(out + used, buf + body, lacing[i]);
            used += lacing[i];
            body += lacing[i];
            if (lacing[i] < 255) {
                if (index == 0) {
                    if (used - start < AUDF_OPUS_HEAD_BYTES) return false;
                    memcpy(s->head, out + start, AUDF_OPUS_HEAD_BYTES);
                } else if (index > 1) {
                    push(s, out + start, used - start, 0);
                }
                index++;
                start = used;
            }
        }
        if (granule >= 0) s->last_granule = granule;
        p = body;
    }
    s->ogg = true;
    return index > 2;
}

int main(int argc, char **argv) {
    const char *in = NULL, *out = NULL, *ref = NULL;
    int channels = 2;
    int loops = 1;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--mono") == 0) {
            channels = 1;
        } else if (strcmp(argv[i], "--loops") == 0 && i + 1 < argc) {
            loops = atoi(argv[++i]);
        } else if (strcmp(argv[i], "--ref") == 0 && i + 1 < argc) {
            ref = argv[++i];
        } else if (!in) {
            in = argv[i];
        } else {
            out = argv[i];
        }
    }
    if (!in) {
        fprintf(stderr, "usage: %s [--mono] [--loops n] [--ref ref.raw] in.bit|in.opus [out.raw]\n",
                argv[0]);
        return 2;
    }
    size_t size;
    uint8_t *buf = read_file(in, &size);
    if (!buf) {
        fprintf(stderr, "cannot read %s\n", in);
        return 2;
    }
    stream_t s = { 0 };
    uint8_t *joined = NULL;
    const bool ok = size >= 4 && memcmp(buf, "OggS", 4) == 0 ? parse_ogg(&s, buf, size, &joined)
                                                             : parse_bit(&s, buf, size, channels);
    if (!ok) {
        fprintf(stderr, "cannot parse %s\n", in);
        return 2;
    }

    audf_decoder_t *dec;
    const audf_opus_config_t config = { .head = s.head, .head_len = sizeof(s.head) };
    esp_err_t err = audf_opus_decoder_create(&config, &dec);
    if (err != ESP_OK) {
        fprintf(stderr, "create: %d\n", err);
        return 2;
    }
    const int ch = audf_decoder_channels(dec);
    int16_t *pcm = malloc(audf_decoder_max_frames(dec) * ch * sizeof(int16_t));
    size_t ref_size = 0;
    uint8_t *ref_buf = ref ? read_file(ref, &ref_size) : NULL;
    if (ref && !ref_buf) {
        fprintf(stderr, "cannot read %s\n", ref);
        return 2;
    }
    const int16_t *ref_pcm = (const int16_t *)ref_buf;
    const size_t ref_samples = ref_size / sizeof(int16_t);
    double signal = 0, noise = 0;
    int max_diff = 0;
    FILE *f = out ? fopen(out, "wb") : NULL;
    size_t mismatches = 0, errors = 0;
    int64_t written = 0;
    const int64_t skip = s.ogg ? audf_opus_decoder_pre_skip(dec) : 0;
    const int64_t total = s.ogg ? s.last_granule - skip : INT64_MAX;
    const clock_t t0 = clock();
    for (int loop = 0; loop < loops; loop++) {
        audf_decoder_reset(dec);
        int64_t position = 0;
        for (size_t i = 0; i < s.count; i++) {
            size_t frames = 0;
            err = audf_decoder_decode(dec, s.packets[i].data, s.packets[i].size, pcm, &frames);
            if (err != ESP_OK) {
                if (loop == 0) errors++;
                continue;
            }
            if (loop == 0 && !s.ogg && audf_opus_decoder_final_range(dec) != s.ranges[i]) {
                if (mismatches < 5) {
                    fprintf(stderr, "packet %zu: final range %08x, expected %08x\n", i,
                            audf_opus_decoder_final_range(dec), s.ranges[i]);
                }
                mismatches++;
            }
            if (loop == 0) {
                int64_t from = position < skip ? skip - position : 0;
                int64_t to = (int64_t)frames;
                if (position + to - skip > total) to = total + skip - position;
                for (int64_t k = from * ch; ref_pcm && k < to * ch; k++) {
                    const size_t at = (size_t)((written + k / ch - from) * ch + k % ch);
                    if (at >= ref_samples) break;
                    const int d = pcm[k] - ref_pcm[at];
                    signal += (double)ref_pcm[at] * ref_pcm[at];
                    noise += (double)d * d;
                    if (abs(d) > max_diff) max_diff = abs(d);
                }
                if (from < to) {
                    if (f) fwrite(pcm + from * ch, sizeof(int16_t) * ch, (size_t)(to - from), f);
                    written += to - from;
                }
            }
            position += (int64_t)frames;
        }
    }
    const double seconds = (double)(clock() - t0) / CLOCKS_PER_SEC;
    if (f) fclose(f);
    printf("%zu packets, %lld frames written, %.3f s", s.count, (long long)written, seconds);
    if (!s.ogg) printf(", %zu range mismatches", mismatches);
    printf(", %zu errors\n", errors);
    if (ref_pcm) {
        printf("against %s: SNR %.1f dB, max difference %d%s\n", ref,
               10 * log10((signal + 1) / (noise + 1)), max_diff,
               (size_t)written * ch == ref_samples ? "" : ", lengths differ");
    }
    audf_decoder_destroy(dec);
    free(pcm);
    free(joined);
    free(ref_buf);
    free(buf);
    return mismatches || errors ? 1 : 0;
}
