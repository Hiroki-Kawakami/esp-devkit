/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host unit tests for compression_framework. Compiled by test/run.sh.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cmpf_inflate.h"
#include "cmpf_lzw.h"

#include "cmpf_fixtures.h"

static int g_failures = 0;

#define CHECK(cond)                                                       \
    do {                                                                  \
        if (!(cond)) {                                                    \
            fprintf(stderr, "FAIL %s:%d  %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                 \
        }                                                                 \
    } while (0)

static void noise(uint8_t *out, size_t n, uint8_t mask) {
    uint32_t x = 12345;
    for (size_t i = 0; i < n; i++) {
        x = (x * 1103515245u + 12345u) & 0x7FFFFFFFu;
        out[i] = (uint8_t)((x >> 16) & mask);
    }
}

static void runs(uint8_t *out, size_t n) {
    for (size_t i = 0; i < n; i++) out[i] = (i / 300) % 2 ? (uint8_t)(i / 300) : 0;
}

typedef struct {
    const uint8_t *data;
    size_t len;
    size_t pos;
    size_t chunk;
} chunk_source_t;

static const uint8_t *chunk_refill(void *user, size_t *n) {
    chunk_source_t *s = (chunk_source_t *)user;
    size_t k = s->len - s->pos;
    if (k > s->chunk) k = s->chunk;
    *n = k;
    const uint8_t *p = s->data + s->pos;
    s->pos += k;
    return p;
}

static uint8_t g_out[8192];
static uint8_t g_window[CMPF_INFLATE_WINDOW_BYTES];
static uint16_t g_work[CMPF_LZW_WORK_BYTES / 2];

static size_t inflate_all(const uint8_t *data, size_t len, size_t in_chunk, size_t out_chunk,
                          bool *failed) {
    chunk_source_t s = { data, len, 0, in_chunk };
    cmpf_inflate_t z;
    if (!cmpf_inflate_init(&z, (cmpf_src_t){ chunk_refill, &s }, g_window)) {
        *failed = true;
        return 0;
    }
    size_t total = 0;
    while (total < sizeof g_out) {
        size_t want = sizeof g_out - total;
        if (want > out_chunk) want = out_chunk;
        size_t got = cmpf_inflate_read(&z, g_out + total, want);
        total += got;
        if (got < want) break;
    }
    *failed = cmpf_inflate_failed(&z);
    return total;
}

static size_t lzw_all(const uint8_t *data, size_t len, const cmpf_lzw_opts_t *opts,
                      size_t in_chunk, size_t out_chunk, bool *failed) {
    chunk_source_t s = { data, len, 0, in_chunk };
    cmpf_lzw_t z;
    if (!cmpf_lzw_init(&z, (cmpf_src_t){ chunk_refill, &s }, opts, g_work)) {
        *failed = true;
        return 0;
    }
    size_t total = 0;
    while (total < sizeof g_out) {
        size_t want = sizeof g_out - total;
        if (want > out_chunk) want = out_chunk;
        size_t got = cmpf_lzw_read(&z, g_out + total, want);
        total += got;
        if (got < want) break;
    }
    *failed = cmpf_lzw_failed(&z);
    return total;
}

static void test_inflate(void) {
    uint8_t want[5000];
    bool failed = false;
    static const size_t kChunks[][2] = { { 4096, 8192 }, { 1, 7 }, { 13, 1 } };
    for (size_t i = 0; i < sizeof kChunks / sizeof kChunks[0]; i++) {
        noise(want, 4096, 0xFF);
        CHECK(inflate_all(kZlibNoise, sizeof kZlibNoise, kChunks[i][0], kChunks[i][1], &failed) ==
              4096);
        CHECK(!failed && memcmp(g_out, want, 4096) == 0);

        runs(want, 5000);
        CHECK(inflate_all(kZlibRuns, sizeof kZlibRuns, kChunks[i][0], kChunks[i][1], &failed) ==
              5000);
        CHECK(!failed && memcmp(g_out, want, 5000) == 0);

        noise(want, 300, 0xFF);
        CHECK(inflate_all(kZlibStored, sizeof kZlibStored, kChunks[i][0], kChunks[i][1],
                          &failed) == 300);
        CHECK(!failed && memcmp(g_out, want, 300) == 0);
    }
}

static void test_inflate_rejects_bad_header(void) {
    uint8_t bad[sizeof kZlibRuns];
    memcpy(bad, kZlibRuns, sizeof bad);
    bad[1] ^= 1;
    chunk_source_t s = { bad, sizeof bad, 0, sizeof bad };
    cmpf_inflate_t z;
    CHECK(!cmpf_inflate_init(&z, (cmpf_src_t){ chunk_refill, &s }, g_window));
}

static void test_lzw_gif(void) {
    static const cmpf_lzw_opts_t kGif8 = { 8, 0, 0 };
    static const cmpf_lzw_opts_t kGif2 = { 2, 0, 0 };
    uint8_t want[5000];
    bool failed = false;
    static const size_t kChunks[][2] = { { 4096, 8192 }, { 1, 5 }, { 255, 1 } };
    for (size_t i = 0; i < sizeof kChunks / sizeof kChunks[0]; i++) {
        noise(want, 4096, 0xFF);
        CHECK(lzw_all(kGifNoise, sizeof kGifNoise, &kGif8, kChunks[i][0], kChunks[i][1],
                      &failed) == 4096);
        CHECK(!failed && memcmp(g_out, want, 4096) == 0);

        runs(want, 5000);
        CHECK(lzw_all(kGifRuns, sizeof kGifRuns, &kGif8, kChunks[i][0], kChunks[i][1], &failed) ==
              5000);
        CHECK(!failed && memcmp(g_out, want, 5000) == 0);

        noise(want, 1200, 0x03);
        CHECK(lzw_all(kGifNarrow, sizeof kGifNarrow, &kGif2, kChunks[i][0], kChunks[i][1],
                      &failed) == 1200);
        CHECK(!failed && memcmp(g_out, want, 1200) == 0);
    }
}

static void test_lzw_tiff(void) {
    static const cmpf_lzw_opts_t kTiff = { 8, 1, 1 };
    uint8_t want[5000];
    bool failed = false;
    static const size_t kChunks[][2] = { { 4096, 8192 }, { 1, 3 } };
    for (size_t i = 0; i < sizeof kChunks / sizeof kChunks[0]; i++) {
        noise(want, 4096, 0xFF);
        CHECK(lzw_all(kTiffNoise, sizeof kTiffNoise, &kTiff, kChunks[i][0], kChunks[i][1],
                      &failed) == 4096);
        CHECK(!failed && memcmp(g_out, want, 4096) == 0);

        runs(want, 5000);
        CHECK(lzw_all(kTiffRuns, sizeof kTiffRuns, &kTiff, kChunks[i][0], kChunks[i][1],
                      &failed) == 5000);
        CHECK(!failed && memcmp(g_out, want, 5000) == 0);
    }
}

static void test_lzw_reinit_reuses_work(void) {
    static const cmpf_lzw_opts_t kTiff = { 8, 1, 1 };
    uint8_t want[5000];
    bool failed = false;
    runs(want, 5000);
    for (int i = 0; i < 2; i++) {
        CHECK(lzw_all(kTiffRuns, sizeof kTiffRuns, &kTiff, 64, 64, &failed) == 5000);
        CHECK(!failed && memcmp(g_out, want, 5000) == 0);
    }
}

static void test_lzw_rejects_bad_code(void) {
    static const cmpf_lzw_opts_t kGif2 = { 2, 0, 0 };
    /* clear (4), then 7: a code past the first free entry. */
    static const uint8_t kBad[] = { 0x3C, 0x00 };
    bool failed = false;
    lzw_all(kBad, sizeof kBad, &kGif2, 16, 16, &failed);
    CHECK(failed);

    cmpf_lzw_t z;
    static const cmpf_lzw_opts_t kWide = { 9, 0, 0 };
    chunk_source_t s = { kBad, sizeof kBad, 0, 1 };
    CHECK(!cmpf_lzw_init(&z, (cmpf_src_t){ chunk_refill, &s }, &kWide, g_work));
}

int main(void) {
    test_inflate();
    test_inflate_rejects_bad_header();
    test_lzw_gif();
    test_lzw_tiff();
    test_lzw_reinit_reuses_work();
    test_lzw_rejects_bad_code();

    if (g_failures) {
        fprintf(stderr, "\n%d failure(s)\n", g_failures);
        return 1;
    }
    printf("all tests passed\n");
    return 0;
}
