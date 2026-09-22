/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_bench.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_decoder.h"
#include "imgf_encoder.h"
#include "imgf_jpegd.h"
#include "imgf_jpege.h"
#include "imgf_pngd.h"
#include "imgf_resize.h"
#include "imgf_stream.h"
#include "imgf_types.h"

#include "imgf_bench_assets.h"

#ifdef ESP_PLATFORM
#include "esp_timer.h"
static uint64_t now_us(void) { return (uint64_t)esp_timer_get_time(); }
#else
#include <time.h>
static uint64_t now_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000u + (uint64_t)ts.tv_nsec / 1000u;
}
#endif

#define SRC_W 1920
#define SRC_H 1080
#define REPS  3

static const int kQuality[2] = {85, 95};

typedef struct {
    uint8_t *src;          /* SRC_W x SRC_H RGB888 */
    uint8_t *jpeg[2];      /* baseline 4:2:0 at kQuality[] */
    size_t   jpeg_len[2];
    uint8_t *dst;          /* one destination image, sized for every case */
    size_t   dst_cap;
} bench_t;

typedef struct {
    const uint8_t *data;
    size_t         len;
} blob_t;

typedef imgf_decoder_t *(*open_fn)(const blob_t *, imgf_buffer_source_t *,
                                   const imgf_decode_opts_t *);

/* --- source picture -------------------------------------------------------
 * Integer-only so it costs the same on every target, and shaped like the one
 * gen_assets.py draws: a low-frequency colour field, near-Nyquist texture and
 * grain. The texture is what gives the entropy stages realistic work — plain
 * grain is quantised away at normal JPEG quality. */

static int16_t g_sin[256];   /* q12 sine, index = 1/256 turn */

static void sin_table(void) {
    for (int i = 0; i < 128; i++) {
        g_sin[i]       = (int16_t)(i * (128 - i));
        g_sin[i + 128] = (int16_t)(-i * (128 - i));
    }
}

static inline int isin(int a) { return g_sin[(unsigned)a & 255]; }

static inline uint8_t clamp8i(int v) { return v < 0 ? 0 : (v > 255 ? 255 : (uint8_t)v); }

static void fill_source(uint8_t *p) {
    uint32_t s = 2463534242u;
    for (int y = 0; y < SRC_H; y++) {
        const int ay = y * 2048 / SRC_H;                 /* 8 turns top to bottom */
        for (int x = 0; x < SRC_W; x++) {
            const int ax = x * 2304 / SRC_W;             /* 9 turns left to right */
            const int lo = (isin(ax) + isin(ay + 53) + isin((ax + ay) >> 1) / 2) >> 6;
            const int tex = (((isin(x * 85) * isin(y * 69)) >> 12) +
                             ((isin((x + y) * 46) * 3) >> 2)) * 5 >> 12;
            s ^= s << 13; s ^= s >> 17; s ^= s << 5;
            const int n = ((int)((s >> 8) & 0x3F) - 32) / 8;
            p[0] = clamp8i(128 + lo + x * 60 / SRC_W + tex + n);
            p[1] = clamp8i(128 + (isin(lo * 2) >> 6) + y * 50 / SRC_H + tex + n);
            p[2] = clamp8i(128 + (isin(lo + 64) >> 6) - tex + n);
            p += 3;
        }
    }
}

/* --- reporting ------------------------------------------------------------ */

/* Every case prints a checksum of its output. The host build has no PIE
 * kernels, so a device run whose checksums match the host's is the proof that
 * a kernel writes what its C counterpart does. */
static uint32_t g_sum;

static void sum_init(void) { g_sum = 2166136261u; }

static void sum_add(const void *p, size_t n) {
    const uint8_t *b = (const uint8_t *)p;
    uint32_t h = g_sum;
    while (n--) { h = (h ^ *b++) * 16777619u; }
    g_sum = h;
}

/* Integer formatting throughout: the target's newlib is built without float
 * support in printf. */
static void report(const char *name, uint64_t us, uint32_t out_px) {
    const unsigned mpx100 = us ? (unsigned)(((uint64_t)out_px * 100) / us) : 0;
    printf("  %-48s %6u.%02u ms  %3u.%02u Mpx/s  %08x\n", name,
           (unsigned)(us / 1000), (unsigned)(us % 1000) / 10,
           mpx100 / 100, mpx100 % 100, (unsigned)g_sum);
}

static void failed(const char *name, const char *why) {
    printf("  %-48s FAILED (%s)\n", name, why);
}

/* --- decoders ------------------------------------------------------------- */

static imgf_decoder_t *open_jpeg(const blob_t *b, imgf_buffer_source_t *bs,
                                 const imgf_decode_opts_t *opts) {
    imgf_decoder_t *d = imgf_jpegd_create();
    if (!d) return NULL;
    if (imgf_decoder_open(d, imgf_stream_from_buffer(bs, b->data, b->len), opts) != IMGF_OK) {
        imgf_decoder_destroy(d);
        return NULL;
    }
    return d;
}

static imgf_decoder_t *open_png(const blob_t *b, imgf_buffer_source_t *bs,
                                const imgf_decode_opts_t *opts) {
    imgf_decoder_t *d = imgf_pngd_create();
    if (!d) return NULL;
    if (imgf_decoder_open(d, imgf_stream_from_buffer(bs, b->data, b->len), opts) != IMGF_OK) {
        imgf_decoder_destroy(d);
        return NULL;
    }
    return d;
}

/* --- cases ---------------------------------------------------------------- */

static void bench_decode(const char *label, const blob_t *blob, open_fn open,
                         const imgf_decode_opts_t *opts) {
    imgf_buffer_source_t bs;
    imgf_decoder_t *probe = open(blob, &bs, opts);
    if (!probe) { failed(label, "open"); return; }
    const uint16_t w = imgf_decoder_width(probe), h = imgf_decoder_height(probe);
    const int bpp = imgf_pixfmt_bpp(imgf_decoder_pixfmt(probe));
    imgf_decoder_destroy(probe);

    char name[80];
    snprintf(name, sizeof name, "%s decode -> %ux%u", label, w, h);

    uint8_t *row = (uint8_t *)imgf_alloc((size_t)w * bpp, 0);
    if (!row) { failed(name, "oom"); return; }

    uint64_t best = (uint64_t)-1;
    imgf_err_t err = IMGF_OK;
    sum_init();
    for (int i = 0; i < REPS; i++) {
        const int keep = i == 0;
        const uint64_t t0 = now_us();
        imgf_decoder_t *d = open(blob, &bs, opts);
        if (!d) { err = IMGF_ERR_DECODE; break; }
        while (imgf_decoder_next_row(d, row)) {
            if (keep) sum_add(row, (size_t)w * bpp);
        }
        err = imgf_decoder_last_error(d);
        imgf_decoder_destroy(d);
        const uint64_t dt = now_us() - t0;
        if (err != IMGF_OK) break;
        if (dt < best) best = dt;
    }
    imgf_free(row);

    if (err != IMGF_OK) failed(name, imgf_err_to_str(err));
    else                report(name, best, (uint32_t)w * h);
}

static const char *pf_name(imgf_pixfmt_t pf) {
    switch (pf) {
        case IMGF_PIX_RGB565: return "RGB565";
        case IMGF_PIX_BGR888: return "BGR888";
        case IMGF_PIX_GRAY8:  return "GRAY8";
        default:              return "RGB888";
    }
}

static void bench_decode_resize(const char *label, const blob_t *blob,
                                uint16_t tw, uint16_t th, imgf_pixfmt_t dst_pf) {
    char name[80];
    snprintf(name, sizeof name, "%s decode+resize -> %ux%u %s",
             label, tw, th, pf_name(dst_pf));

    uint64_t best = (uint64_t)-1;
    imgf_err_t err = IMGF_OK;
    uint32_t px = 0;
    for (int i = 0; i < REPS; i++) {
        imgf_buffer_source_t bs;
        const imgf_decode_opts_t dopts = {.target_w = tw, .target_h = th};
        const uint64_t t0 = now_us();
        imgf_decoder_t *d = open_jpeg(blob, &bs, &dopts);
        if (!d) { err = IMGF_ERR_DECODE; break; }
        const imgf_resize_opts_t ro = {.target_w = tw, .target_h = th, .dst_pixfmt = dst_pf};
        uint8_t *out = NULL;
        uint16_t ow = 0, oh = 0;
        err = imgf_resize_decoder(d, &ro, &out, &ow, &oh);
        imgf_decoder_destroy(d);
        const uint64_t dt = now_us() - t0;
        if (i == 0 && err == IMGF_OK) {
            sum_init();
            sum_add(out, (size_t)ow * oh * imgf_pixfmt_bpp(dst_pf));
        }
        imgf_free(out);
        if (err != IMGF_OK) break;
        px = (uint32_t)ow * oh;
        if (dt < best) best = dt;
    }
    if (err != IMGF_OK) failed(name, imgf_err_to_str(err));
    else                report(name, best, px);
}

/* `where` distinguishes the PSRAM cases from the small internal-SRAM one, which
 * is there to show how much of the resize time is memory rather than compute:
 * the two use the same scale ratio, so their Mpx/s are comparable. */
static void bench_resize_buf(const uint8_t *src, size_t stride, uint8_t *dst,
                             uint16_t sw, uint16_t sh, uint16_t dw, uint16_t dh,
                             imgf_pixfmt_t dst_pf, const char *where, int reps) {
    char name[80];
    snprintf(name, sizeof name, "resize%s %ux%u -> %ux%u %s",
             where, sw, sh, dw, dh, pf_name(dst_pf));

    uint64_t best = (uint64_t)-1;
    imgf_err_t err = IMGF_OK;
    for (int i = 0; i < reps; i++) {
        const imgf_resize_opts_t ro = {.target_w = dw, .target_h = dh,
                                       .fit = IMGF_FIT_STRETCH, .dst_pixfmt = dst_pf};
        const uint64_t t0 = now_us();
        err = imgf_resize_buffer(src, sw, sh, stride, IMGF_PIX_RGB888, dst, 0, &ro);
        const uint64_t dt = now_us() - t0;
        if (err != IMGF_OK) break;
        if (i == 0) {
            sum_init();
            sum_add(dst, (size_t)dw * dh * imgf_pixfmt_bpp(dst_pf));
        }
        if (dt < best) best = dt;
    }
    if (err != IMGF_OK) failed(name, imgf_err_to_str(err));
    else                report(name, best, (uint32_t)dw * dh);
}

static void bench_resize(const bench_t *b, uint16_t sw, uint16_t sh,
                         uint16_t dw, uint16_t dh, imgf_pixfmt_t dst_pf) {
    if ((size_t)dw * dh * imgf_pixfmt_bpp(dst_pf) > b->dst_cap) {
        char name[80];
        snprintf(name, sizeof name, "resize %ux%u -> %ux%u %s",
                 sw, sh, dw, dh, pf_name(dst_pf));
        failed(name, "dst too large");
        return;
    }
    bench_resize_buf(b->src, (size_t)SRC_W * 3, b->dst, sw, sh, dw, dh, dst_pf, "", REPS);
}

#define INT_SW 256
#define INT_SH 144
#define INT_DW 171
#define INT_DH  96

static void bench_resize_internal(const bench_t *b) {
    uint8_t *src = (uint8_t *)imgf_alloc_internal((size_t)INT_SW * INT_SH * 3);
    uint8_t *dst = (uint8_t *)imgf_alloc_internal((size_t)INT_DW * INT_DH * 3);
    if (!src || !dst) {
        failed("resize (internal) 256x144 -> 171x96 RGB888", "no internal memory");
    } else {
        for (int y = 0; y < INT_SH; y++)
            memcpy(src + (size_t)y * INT_SW * 3, b->src + (size_t)y * SRC_W * 3,
                   (size_t)INT_SW * 3);
        bench_resize_buf(src, (size_t)INT_SW * 3, dst, INT_SW, INT_SH, INT_DW, INT_DH,
                         IMGF_PIX_RGB888, " (internal)", 30);
    }
    imgf_free(src);
    imgf_free(dst);
}

/* --- driver --------------------------------------------------------------- */

static bool build_fixture(bench_t *b) {
    sin_table();
    b->src = (uint8_t *)imgf_alloc((size_t)SRC_W * SRC_H * 3, 0);
    if (!b->src) return false;
    fill_source(b->src);

    /* The encoder's upper bound is 2 bytes per sample — 12 MB here, twice over
     * — so it is borrowed for one encode at a time and the result copied down
     * to its real size. The destination image is allocated after, when that
     * scratch is gone. */
    const size_t cap = imgf_jpege_buffer_upper_bound(SRC_W, SRC_H, IMGF_PIX_RGB888);
    uint8_t *scratch = (uint8_t *)imgf_alloc(cap, 0);
    if (!scratch) return false;

    for (int q = 0; q < 2; q++) {
        const imgf_jpege_opts_t eo = {.quality = kQuality[q],
                                      .subsample = IMGF_JPEG_SUBSAMPLE_420};
        imgf_err_t e = IMGF_ERR_OOM;
        size_t len = 0;
        imgf_encoder_t *enc = imgf_jpege_create(SRC_W, SRC_H, IMGF_PIX_RGB888, &eo, &e);
        if (enc) {
            e = imgf_encoder_encode_buffer(enc, b->src, (size_t)SRC_W * 3,
                                           scratch, cap, &len);
            imgf_encoder_destroy(enc);
        }
        if (e == IMGF_OK && len) {
            b->jpeg[q] = (uint8_t *)imgf_alloc(len, 0);
            if (b->jpeg[q]) {
                memcpy(b->jpeg[q], scratch, len);
                b->jpeg_len[q] = len;
            }
        }
        if (!b->jpeg_len[q]) {
            printf("  fixture: baseline q%d encode failed (%s)\n",
                   kQuality[q], imgf_err_to_str(e));
            continue;
        }
        const unsigned bpp100 = (unsigned)((b->jpeg_len[q] * 800) / ((size_t)SRC_W * SRC_H));
        printf("  fixture: baseline 4:2:0 q%-3d %8u B  (%u.%02u bpp)\n",
               kQuality[q], (unsigned)b->jpeg_len[q], bpp100 / 100, bpp100 % 100);
    }
    imgf_free(scratch);

    b->dst_cap = (size_t)SRC_W * SRC_H * 3;
    b->dst = (uint8_t *)imgf_alloc(b->dst_cap, 0);
    if (!b->dst) return false;

    printf("  fixture: asset %dx%d, png %u B, progressive jpeg %s\n",
           IMGF_BENCH_ASSET_W, IMGF_BENCH_ASSET_H, (unsigned)sizeof kBenchPng,
           IMGF_BENCH_HAS_PROGRESSIVE ? "present" : "absent (no cjpeg at build time)");
    return true;
}

void imgf_bench_run(void) {
    bench_t b = {0};
    printf("\nimage_framework bench  (source %dx%d RGB888, best of %d)\n\n",
           SRC_W, SRC_H, REPS);

    if (!build_fixture(&b)) {
        printf("  OOM building the fixture\n");
        goto done;
    }
    printf("\n");

    for (int q = 0; q < 2; q++) {
        if (!b.jpeg_len[q]) continue;
        const blob_t blob = {b.jpeg[q], b.jpeg_len[q]};
        char label[24];
        snprintf(label, sizeof label, "jpeg q%d", kQuality[q]);

        const imgf_decode_opts_t full = {0};
        const imgf_decode_opts_t half = {.target_w = SRC_W / 2, .target_h = SRC_H / 2};
        const imgf_decode_opts_t quarter = {.target_w = SRC_W / 4, .target_h = SRC_H / 4};
        bench_decode(label, &blob, open_jpeg, &full);
        bench_decode(label, &blob, open_jpeg, &half);
        bench_decode(label, &blob, open_jpeg, &quarter);

        bench_decode_resize(label, &blob, 1280, 720, IMGF_PIX_RGB565);
        bench_decode_resize(label, &blob, 1280, 720, IMGF_PIX_RGB888);
        bench_decode_resize(label, &blob, 1280, 720, IMGF_PIX_BGR888);
        bench_decode_resize(label, &blob,  640, 360, IMGF_PIX_RGB565);
        printf("\n");
    }

    {
        const blob_t png = {kBenchPng, sizeof kBenchPng};
        const imgf_decode_opts_t full = {0};
        bench_decode("png", &png, open_png, &full);
    }
    if (IMGF_BENCH_HAS_PROGRESSIVE) {
        const blob_t prog = {kBenchProgJpeg, sizeof kBenchProgJpeg};
        const imgf_decode_opts_t full = {0};
        bench_decode("jpeg progressive q85", &prog, open_jpeg, &full);
    }
    printf("\n");

    bench_resize(&b, SRC_W, SRC_H, 1280, 720, IMGF_PIX_RGB888);
    bench_resize(&b, SRC_W, SRC_H, 1280, 720, IMGF_PIX_RGB565);
    bench_resize(&b, SRC_W, SRC_H,  960, 540, IMGF_PIX_RGB888);
    bench_resize(&b, SRC_W, SRC_H,  640, 360, IMGF_PIX_RGB888);
    bench_resize(&b,   640,  360, 1280,  720, IMGF_PIX_RGB888);
    bench_resize_internal(&b);
    printf("\n");

done:
    for (int q = 0; q < 2; q++) imgf_free(b.jpeg[q]);
    imgf_free(b.dst);
    imgf_free(b.src);
    printf("bench done\n");
}
