/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host unit tests for image_framework — no ESP-IDF, no hardware.
 * Compiled by test/run.sh (gcc, C11). Cases group around: stream wrappers,
 * sniff, decoder vtable lifecycle, JPEG decoder, PNG decoder.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_async.h"
#include "imgf_decoder.h"
#include "imgf_dither.h"
#include "imgf_encoder.h"
#include "imgf_jpegd.h"
#include "imgf_jpege.h"
#include "imgf_pngd.h"
#include "imgf_raw_encoder.h"
#include "imgf_resize.h"
#include "imgf_sniff.h"
#include "imgf_stream.h"
#include "imgf_types.h"

#include "jpeg_fixtures.h"
#include "png_fixtures.h"

static int g_failures = 0;

#define CHECK(cond)                                                       \
    do {                                                                  \
        if (!(cond)) {                                                    \
            fprintf(stderr, "FAIL %s:%d  %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                 \
        }                                                                 \
    } while (0)

static int near_value(int a, int b, int tol) {
    return (a > b ? a - b : b - a) <= tol;
}

/* ---- err -------------------------------------------------------------- */

static void test_err_strings(void) {
    CHECK(strcmp(imgf_err_to_str(IMGF_OK), "Ok") == 0);
    CHECK(strcmp(imgf_err_to_str(IMGF_ERR_TRUNCATED), "Truncated") == 0);
    CHECK(strcmp(imgf_err_to_str(IMGF_ERR_UNSUPPORTED), "UnsupportedFormat") == 0);
    CHECK(strcmp(imgf_err_to_str(IMGF_ERR_TOO_LARGE), "TooLarge") == 0);
    CHECK(strcmp(imgf_err_to_str(999), "Unknown") == 0);
}

/* ---- stream wrappers --------------------------------------------------- */

static void test_buffer_stream_read(void) {
    const uint8_t src[] = {1, 2, 3, 4, 5};
    imgf_buffer_source_t st;
    imgf_stream_t s = imgf_stream_from_buffer(&st, src, sizeof src);
    uint8_t buf[3];
    CHECK(s.read(s.user, buf, 3) == 3);
    CHECK(buf[0] == 1 && buf[2] == 3);
    CHECK(s.read(s.user, buf, 3) == 2);
    CHECK(buf[0] == 4 && buf[1] == 5);
    CHECK(s.read(s.user, buf, 3) == 0);
}

static void test_file_stream(void) {
    FILE *fp = tmpfile();
    CHECK(fp != NULL);
    const uint8_t payload[] = {0xDE, 0xAD, 0xBE, 0xEF};
    fwrite(payload, 1, sizeof payload, fp);
    rewind(fp);

    imgf_file_source_t st;
    imgf_stream_t s = imgf_stream_from_file(&st, fp, 0, 0);
    uint8_t got[4];
    CHECK(s.read(s.user, got, 4) == 4);
    CHECK(got[0] == 0xDE && got[3] == 0xEF);
    CHECK(s.read(s.user, got, 4) == 0);
    fclose(fp);
}

/* Decode the JPEG embedded inside a padded file (offset/length sub-range). */
static void test_file_subrange_jpeg(void) {
    FILE *fp = tmpfile();
    CHECK(fp != NULL);
    const uint8_t pad[100] = {0};
    fwrite(pad, 1, sizeof pad, fp);
    fwrite(kJpgGrayBig, 1, sizeof kJpgGrayBig, fp);
    fwrite(pad, 1, sizeof pad, fp);
    fseek(fp, 0, SEEK_SET);

    imgf_file_source_t st;
    imgf_stream_t s = imgf_stream_from_file(&st, fp, (long)sizeof pad, sizeof kJpgGrayBig);
    imgf_decoder_t *d = imgf_jpegd_create();
    CHECK(imgf_decoder_open(d, s, NULL) == IMGF_OK);
    CHECK(imgf_decoder_width(d) == 64);
    CHECK(imgf_decoder_height(d) == 64);
    imgf_decoder_destroy(d);
    fclose(fp);
}

/* Custom callback that hands over one byte per call — exercises the buffered
 * reader's partial-fill path. */
typedef struct { const uint8_t *p; size_t left; } onebyte_state_t;

static int onebyte_read(void *user, void *dst, size_t n) {
    (void)n;
    onebyte_state_t *s = (onebyte_state_t *)user;
    if (s->left == 0) return 0;
    *(uint8_t *)dst = *s->p++;
    s->left--;
    return 1;
}

static void test_custom_stream_callback(void) {
    onebyte_state_t st = { kPngRgb2x2, sizeof kPngRgb2x2 };
    imgf_stream_t s = { onebyte_read, &st };
    imgf_decoder_t *d = imgf_pngd_create();
    CHECK(imgf_decoder_open(d, s, NULL) == IMGF_OK);
    CHECK(imgf_decoder_width(d) == 2 && imgf_decoder_height(d) == 2);
    imgf_decoder_destroy(d);
}

/* ---- sniff ------------------------------------------------------------- */

static void test_sniff(void) {
    const uint8_t png[8]  = {0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A};
    const uint8_t jpeg[3] = {0xFF, 0xD8, 0xFF};
    const uint8_t junk[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    CHECK(imgf_sniff(png, 8) == IMGF_FMT_PNG);
    CHECK(imgf_sniff(jpeg, 3) == IMGF_FMT_JPEG);
    CHECK(imgf_sniff(junk, 8) == IMGF_FMT_UNKNOWN);
    CHECK(imgf_sniff(png, 4) == IMGF_FMT_UNKNOWN);
}

static void test_sniff_factory_dispatch(void) {
    imgf_decoder_t *d;

    d = imgf_make_decoder(imgf_sniff(kJpgRgbRed, sizeof kJpgRgbRed));
    CHECK(d != NULL);
    imgf_decoder_destroy(d);

    d = imgf_make_decoder(imgf_sniff(kPngRgb2x2, sizeof kPngRgb2x2));
    CHECK(d != NULL);
    imgf_decoder_destroy(d);

    d = imgf_make_decoder(IMGF_FMT_UNKNOWN);
    CHECK(d == NULL);
}

/* ---- lifecycle / API hygiene ------------------------------------------- */

static void test_destroy_null(void) {
    imgf_decoder_destroy(NULL);  /* must not crash */
}

static void test_create_then_destroy(void) {
    imgf_decoder_t *d = imgf_jpegd_create();
    CHECK(d != NULL);
    imgf_decoder_destroy(d);

    d = imgf_pngd_create();
    CHECK(d != NULL);
    imgf_decoder_destroy(d);
}

static void test_next_row_before_open(void) {
    imgf_decoder_t *d = imgf_jpegd_create();
    uint8_t row[4];
    CHECK(imgf_decoder_next_row(d, row) == false);
    imgf_decoder_destroy(d);
}

static void test_double_open_rejected(void) {
    imgf_buffer_source_t st;
    imgf_stream_t s = imgf_stream_from_buffer(&st, kJpgGrayFlat, sizeof kJpgGrayFlat);
    imgf_decoder_t *d = imgf_jpegd_create();
    CHECK(imgf_decoder_open(d, s, NULL) == IMGF_OK);
    imgf_buffer_source_t st2;
    imgf_stream_t s2 = imgf_stream_from_buffer(&st2, kJpgGrayFlat, sizeof kJpgGrayFlat);
    CHECK(imgf_decoder_open(d, s2, NULL) == IMGF_ERR_INVALID_STATE);
    imgf_decoder_destroy(d);
}

/* ---- JPEG (approximate pixels) ---------------------------------------- */

static imgf_decoder_t *open_jpeg(const void *bytes, size_t len,
                                 imgf_buffer_source_t *st,
                                 const imgf_decode_opts_t *opts,
                                 imgf_err_t *err) {
    imgf_stream_t s = imgf_stream_from_buffer(st, bytes, len);
    imgf_decoder_t *d = imgf_jpegd_create();
    imgf_err_t e = imgf_decoder_open(d, s, opts);
    if (err) *err = e;
    return d;
}

static void test_jpeg_gray_flat(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(kJpgGrayFlat, sizeof kJpgGrayFlat, &st, NULL, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_width(d) == 16);
    CHECK(imgf_decoder_height(d) == 16);
    CHECK(imgf_decoder_pixfmt(d) == IMGF_PIX_GRAY8);
    uint8_t row[16];
    int rows = 0;
    bool flat = true;
    while (imgf_decoder_next_row(d, row)) {
        for (int x = 0; x < 16; x++) flat = flat && near_value(row[x], 128, 4);
        rows++;
    }
    CHECK(flat);
    CHECK(rows == 16);
    imgf_decoder_destroy(d);
}

static void test_jpeg_gray_split(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(kJpgGraySplit, sizeof kJpgGraySplit, &st, NULL, &err);
    CHECK(err == IMGF_OK);
    uint8_t row[16];
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(near_value(row[2], 40, 25));
    CHECK(near_value(row[13], 220, 25));
    CHECK(row[13] > row[2] + 100);
    imgf_decoder_destroy(d);
}

static void test_jpeg_rgb_red(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(kJpgRgbRed, sizeof kJpgRgbRed, &st, NULL, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_pixfmt(d) == IMGF_PIX_RGB888);
    uint8_t row[16 * 3];
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(near_value(row[0], 220, 20));
    CHECK(near_value(row[1], 30, 25));
    CHECK(near_value(row[2], 30, 25));
    CHECK(row[0] > row[1] + 100 && row[0] > row[2] + 100);
    imgf_decoder_destroy(d);
}

static void test_jpeg_decode_time_downscale(void) {
    imgf_decode_opts_t opts = {0};
    opts.target_w = 16;
    opts.target_h = 16;
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(kJpgGrayBig, sizeof kJpgGrayBig, &st, &opts, &err);
    CHECK(err == IMGF_OK);
    uint16_t w = imgf_decoder_width(d);
    CHECK(w < 64 && w >= 16);
    uint8_t row[64];
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(near_value(row[0], 100, 6));
    imgf_decoder_destroy(d);
}

static void test_jpeg_truncated_header(void) {
    /* SOI only — no SOF, no SOS. open() must report truncation. */
    const uint8_t trunc[] = { 0xFF, 0xD8 };
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(trunc, sizeof trunc, &st, NULL, &err);
    CHECK(err == IMGF_ERR_TRUNCATED);
    CHECK(imgf_decoder_last_error(d) == IMGF_ERR_TRUNCATED);
    imgf_decoder_destroy(d);
}

static void test_jpeg_progressive_unsupported(void) {
    /* SOI + SOF2 (progressive) marker + segment length: the parser reads the
     * length before dispatching, so the bytes after the length aren't needed —
     * non-baseline SOF returns UNSUPPORTED before any segment body is read. */
    const uint8_t progressive[] = { 0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x0B };
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(progressive, sizeof progressive, &st, NULL, &err);
    CHECK(err == IMGF_ERR_UNSUPPORTED);
    imgf_decoder_destroy(d);
}

/* ---- PNG (exact pixels) ----------------------------------------------- */

static imgf_decoder_t *open_png(const void *bytes, size_t len,
                                imgf_buffer_source_t *st,
                                imgf_err_t *err) {
    imgf_stream_t s = imgf_stream_from_buffer(st, bytes, len);
    imgf_decoder_t *d = imgf_pngd_create();
    imgf_err_t e = imgf_decoder_open(d, s, NULL);
    if (err) *err = e;
    return d;
}

static void test_png_gray(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngGray2x2, sizeof kPngGray2x2, &st, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_width(d) == 2 && imgf_decoder_height(d) == 2);
    CHECK(imgf_decoder_pixfmt(d) == IMGF_PIX_GRAY8);
    uint8_t row[2];
    CHECK(imgf_decoder_next_row(d, row) && row[0] == 0 && row[1] == 255);
    CHECK(imgf_decoder_next_row(d, row) && row[0] == 128 && row[1] == 64);
    CHECK(!imgf_decoder_next_row(d, row));
    imgf_decoder_destroy(d);
}

static void test_png_rgb(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngRgb2x2, sizeof kPngRgb2x2, &st, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_pixfmt(d) == IMGF_PIX_RGB888);
    uint8_t row[6];
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(row[0] == 255 && row[1] == 0   && row[2] == 0);
    CHECK(row[3] == 0   && row[4] == 255 && row[5] == 0);
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(row[0] == 0   && row[1] == 0   && row[2] == 255);
    CHECK(row[3] == 255 && row[4] == 255 && row[5] == 255);
    imgf_decoder_destroy(d);
}

static void test_png_palette(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngPalette2x2, sizeof kPngPalette2x2, &st, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_pixfmt(d) == IMGF_PIX_RGB888);
    uint8_t row[6];
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(row[0] == 255 && row[1] == 0   && row[2] == 0);
    CHECK(row[3] == 0   && row[4] == 255 && row[5] == 0);
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(row[0] == 0   && row[1] == 0   && row[2] == 255);
    imgf_decoder_destroy(d);
}

static void test_png_rgba_composite(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngRgba2x2, sizeof kPngRgba2x2, &st, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_pixfmt(d) == IMGF_PIX_RGB888);
    uint8_t row[6];
    CHECK(imgf_decoder_next_row(d, row));
    /* (c*a + 255*(255-a) + 127) / 255, a=128 over white: (200,100,50) -> (227,177,152). */
    CHECK(row[0] == 227 && row[1] == 177 && row[2] == 152);
    CHECK(row[3] == 10  && row[4] == 20  && row[5] == 30);
    CHECK(imgf_decoder_next_row(d, row));
    CHECK(row[0] == 255 && row[1] == 255 && row[2] == 255);
    imgf_decoder_destroy(d);
}

static void test_png_gradient_inflate(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngGrad16, sizeof kPngGrad16, &st, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_decoder_width(d) == 16 && imgf_decoder_height(d) == 16);
    uint8_t row[16];
    int y = 0;
    bool ok = true;
    while (imgf_decoder_next_row(d, row)) {
        for (int x = 0; x < 16; x++) ok = ok && (row[x] == ((x * 16 + y * 4) & 0xFF));
        y++;
    }
    CHECK(ok);
    CHECK(y == 16);
    imgf_decoder_destroy(d);
}

static void test_png_truncated_idat(void) {
    /* Lop off the second half so IDAT inflate runs short. open() may still
     * succeed (signature + IHDR + the start of the first IDAT chunk header are
     * present), but next_row must fail and set IMGF_ERR_TRUNCATED. */
    size_t cut = sizeof kPngGrad16 / 2;
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngGrad16, cut, &st, &err);
    if (err == IMGF_OK) {
        uint8_t row[16];
        bool any_ok = imgf_decoder_next_row(d, row);
        (void)any_ok;
        /* Drain — somewhere along the way next_row returns false. */
        while (imgf_decoder_next_row(d, row)) { /* nothing */ }
        CHECK(imgf_decoder_last_error(d) == IMGF_ERR_TRUNCATED);
    } else {
        /* Either the IHDR walk hit EOF first; that's also fine. */
        CHECK(err == IMGF_ERR_TRUNCATED);
    }
    imgf_decoder_destroy(d);
}

static void test_png_signature_only(void) {
    const uint8_t sig[8] = {0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A};
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(sig, sizeof sig, &st, &err);
    CHECK(err == IMGF_ERR_TRUNCATED);
    imgf_decoder_destroy(d);
}

/* ---- resize ----------------------------------------------------------- */

static void test_pixfmt_bpp(void) {
    CHECK(imgf_pixfmt_bpp(IMGF_PIX_GRAY8) == 1);
    CHECK(imgf_pixfmt_bpp(IMGF_PIX_RGB888) == 3);
    CHECK(imgf_pixfmt_bpp(IMGF_PIX_RGB565) == 2);
    CHECK(imgf_pixfmt_bpp(IMGF_PIX_INHERIT) == 0);
}

static void test_resize_compute_dst(void) {
    imgf_resize_opts_t o = {0};
    o.target_w = 100;
    o.target_h = 50;
    o.fit = IMGF_FIT_STRETCH;
    uint16_t dw, dh;
    CHECK(imgf_resize_compute_dst(200, 100, &o, &dw, &dh) == IMGF_OK);
    CHECK(dw == 100 && dh == 50);

    /* Contain: aspect preserved, fit inside */
    o.fit = IMGF_FIT_CONTAIN;
    o.target_w = 100; o.target_h = 100;
    CHECK(imgf_resize_compute_dst(200, 100, &o, &dw, &dh) == IMGF_OK);
    CHECK(dw == 100 && dh == 50);   /* width-bound */

    o.target_w = 100; o.target_h = 25;
    CHECK(imgf_resize_compute_dst(200, 100, &o, &dw, &dh) == IMGF_OK);
    CHECK(dw == 50 && dh == 25);    /* height-bound */

    /* Stretch with zero target → invalid */
    o.fit = IMGF_FIT_STRETCH;
    o.target_w = 0; o.target_h = 50;
    CHECK(imgf_resize_compute_dst(200, 100, &o, &dw, &dh) == IMGF_ERR_INVALID_ARG);
}

static void test_resize_identity_gray(void) {
    /* sw = dw, sh = dh, src_pf == dst_pf → identity pixels. */
    const uint8_t src[4] = {10, 20, 30, 40};
    uint8_t dst[4] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 2; o.target_h = 2; o.fit = IMGF_FIT_STRETCH;
    CHECK(imgf_resize_buffer(src, 2, 2, 2, IMGF_PIX_GRAY8,
                             dst, 2, &o) == IMGF_OK);
    CHECK(dst[0] == 10 && dst[1] == 20 && dst[2] == 30 && dst[3] == 40);
}

static void test_resize_downscale_gray_2x(void) {
    /* 4x4 source, halved on both axes: each dst pixel is the mean of a 2x2
     * source block. */
    uint8_t src[16];
    for (int i = 0; i < 16; i++) src[i] = (uint8_t)(i * 10);
    uint8_t dst[4] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 2; o.target_h = 2; o.fit = IMGF_FIT_STRETCH;
    CHECK(imgf_resize_buffer(src, 4, 4, 4, IMGF_PIX_GRAY8, dst, 2, &o) == IMGF_OK);
    /* dst[0] = mean(src[0,1,4,5]) = mean(0,10,40,50) = 25 */
    CHECK(dst[0] == 25);
    /* dst[1] = mean(src[2,3,6,7]) = mean(20,30,60,70) = 45 */
    CHECK(dst[1] == 45);
    /* dst[2] = mean(src[8,9,12,13]) = mean(80,90,120,130) = 105 */
    CHECK(dst[2] == 105);
    /* dst[3] = mean(src[10,11,14,15]) = mean(100,110,140,150) = 125 */
    CHECK(dst[3] == 125);
}

static void test_resize_upscale_gray_2x(void) {
    /* Endpoint-aligned bilinear: dst corners = src corners. */
    const uint8_t src[4] = {0, 100, 200, 220};
    uint8_t dst[16] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 4; o.target_h = 4; o.fit = IMGF_FIT_STRETCH;
    CHECK(imgf_resize_buffer(src, 2, 2, 2, IMGF_PIX_GRAY8, dst, 4, &o) == IMGF_OK);
    /* Corners pinned */
    CHECK(dst[0] == 0);
    CHECK(dst[3] == 100);
    CHECK(dst[12] == 200);
    CHECK(dst[15] == 220);
    /* Top edge: dst[0..3] interpolates 0..100; dst[1] ~ 33, dst[2] ~ 67 */
    CHECK(dst[1] >= 30 && dst[1] <= 36);
    CHECK(dst[2] >= 64 && dst[2] <= 70);
}

static void test_resize_rgb888_downscale(void) {
    uint8_t src[2 * 2 * 3] = {
        255,   0,   0,    0, 255,   0,   /* row 0: red,  green */
          0,   0, 255,  100, 100, 100,   /* row 1: blue, gray */
    };
    uint8_t dst[3] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 1; o.target_h = 1; o.fit = IMGF_FIT_STRETCH;
    CHECK(imgf_resize_buffer(src, 2, 2, 6, IMGF_PIX_RGB888, dst, 0, &o) == IMGF_OK);
    /* Each channel is the box-area mean of the 4 source pixels:
       R = (255+0+0+100)/4 = 88, G = (0+255+0+100)/4 = 88, B = (0+0+255+100)/4 = 88 */
    CHECK(dst[0] >= 87 && dst[0] <= 89);
    CHECK(dst[1] >= 87 && dst[1] <= 89);
    CHECK(dst[2] >= 87 && dst[2] <= 89);
}

static void test_resize_rgb565_roundtrip(void) {
    /* RGB888 → RGB565 → RGB888 should preserve the 5/6/5 quantization. */
    uint8_t src[3] = { 0xF8, 0xFC, 0xF8 };  /* high 5/6/5 bits, low bits cleared */
    uint8_t mid[2] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 1; o.target_h = 1; o.fit = IMGF_FIT_STRETCH;
    o.dst_pixfmt = IMGF_PIX_RGB565;
    CHECK(imgf_resize_buffer(src, 1, 1, 3, IMGF_PIX_RGB888, mid, 0, &o) == IMGF_OK);
    /* RGB565 of (0xF8, 0xFC, 0xF8) = (0x1F << 11) | (0x3F << 5) | 0x1F = 0xFFFF */
    const uint16_t *m = (const uint16_t *)mid;
    CHECK(*m == 0xFFFF);

    uint8_t back[3] = {0};
    o.dst_pixfmt = IMGF_PIX_RGB888;
    CHECK(imgf_resize_buffer(mid, 1, 1, 2, IMGF_PIX_RGB565, back, 0, &o) == IMGF_OK);
    /* 0x1F -> 0xFF (replicate low bits), 0x3F -> 0xFF */
    CHECK(back[0] == 0xFF && back[1] == 0xFF && back[2] == 0xFF);
}

static void test_resize_gray_to_rgb888(void) {
    const uint8_t src[1] = { 200 };
    uint8_t dst[3] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 1; o.target_h = 1; o.fit = IMGF_FIT_STRETCH;
    o.dst_pixfmt = IMGF_PIX_RGB888;
    CHECK(imgf_resize_buffer(src, 1, 1, 1, IMGF_PIX_GRAY8, dst, 0, &o) == IMGF_OK);
    CHECK(dst[0] == 200 && dst[1] == 200 && dst[2] == 200);
}

static void test_resize_rgb888_to_gray(void) {
    const uint8_t src[3] = { 100, 200, 50 };
    uint8_t dst[1] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 1; o.target_h = 1; o.fit = IMGF_FIT_STRETCH;
    o.dst_pixfmt = IMGF_PIX_GRAY8;
    CHECK(imgf_resize_buffer(src, 1, 1, 3, IMGF_PIX_RGB888, dst, 0, &o) == IMGF_OK);
    /* BT.709 luma: 100*54 + 200*183 + 50*19 + 128 = 5400 + 36600 + 950 + 128 = 43078
       >> 8 = 168 */
    CHECK(dst[0] >= 165 && dst[0] <= 171);
}

static void test_resize_inherit_pixfmt(void) {
    /* dst_pixfmt = 0 (INHERIT) → keep src pixfmt. */
    const uint8_t src[2] = { 50, 150 };
    uint8_t dst[2] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 2; o.target_h = 1; o.fit = IMGF_FIT_STRETCH;
    /* o.dst_pixfmt left as 0 (IMGF_PIX_INHERIT) */
    CHECK(imgf_resize_buffer(src, 2, 1, 0, IMGF_PIX_GRAY8, dst, 0, &o) == IMGF_OK);
    CHECK(dst[0] == 50 && dst[1] == 150);
}

static void test_resize_mixed_axis(void) {
    /* Horizontal downscale (4 → 2), vertical upscale (2 → 4). */
    uint8_t src[8] = {
        10, 30, 50, 70,
        90, 110, 130, 150,
    };
    uint8_t dst[8] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 2; o.target_h = 4; o.fit = IMGF_FIT_STRETCH;
    CHECK(imgf_resize_buffer(src, 4, 2, 4, IMGF_PIX_GRAY8, dst, 2, &o) == IMGF_OK);
    /* Top dst row should ≈ mean of src row 0 pairs: (10+30)/2=20, (50+70)/2=60 */
    CHECK(dst[0] == 20);
    CHECK(dst[1] == 60);
    /* Bottom dst row ≈ mean of src row 1 pairs: (90+110)/2=100, (130+150)/2=140 */
    CHECK(dst[6] == 100);
    CHECK(dst[7] == 140);
}

static void test_resize_buffer_inplace_downscale(void) {
    /* In-place 4x4 → 2x2 grayscale (dst == src). */
    uint8_t buf[16];
    for (int i = 0; i < 16; i++) buf[i] = (uint8_t)(i * 10);
    imgf_resize_opts_t o = {0};
    o.target_w = 2; o.target_h = 2; o.fit = IMGF_FIT_STRETCH;
    CHECK(imgf_resize_buffer(buf, 4, 4, 4, IMGF_PIX_GRAY8, buf, 2, &o) == IMGF_OK);
    CHECK(buf[0] == 25 && buf[1] == 45 && buf[2] == 105 && buf[3] == 125);
}

static void test_resize_buffer_overlap_upscale_rejected(void) {
    uint8_t buf[16] = {0};
    imgf_resize_opts_t o = {0};
    o.target_w = 4; o.target_h = 4; o.fit = IMGF_FIT_STRETCH;
    /* 2x2 → 4x4 with overlap → must reject */
    CHECK(imgf_resize_buffer(buf, 2, 2, 2, IMGF_PIX_GRAY8, buf, 4, &o) == IMGF_ERR_INVALID_ARG);
}

static void test_resize_decoder_png(void) {
    /* Run kPngGrad16 (16x16) through resize → 8x8 mean-blurred. */
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_png(kPngGrad16, sizeof kPngGrad16, &st, &err);
    CHECK(err == IMGF_OK);
    imgf_resize_opts_t o = {0};
    o.target_w = 8; o.target_h = 8; o.fit = IMGF_FIT_STRETCH;
    uint8_t *out = NULL;
    uint16_t ow = 0, oh = 0;
    CHECK(imgf_resize_decoder(d, &o, &out, &ow, &oh) == IMGF_OK);
    CHECK(ow == 8 && oh == 8);
    CHECK(out != NULL);
    /* Each dst[(x,y)] = mean over 2x2 of grad(x*2..x*2+1, y*2..y*2+1)
       where grad(X,Y) = (X*16 + Y*4) & 0xFF. */
    bool ok = true;
    for (int y = 0; y < 8 && ok; y++) {
        for (int x = 0; x < 8 && ok; x++) {
            int sum = 0;
            for (int yy = 0; yy < 2; yy++)
                for (int xx = 0; xx < 2; xx++)
                    sum += ((x * 2 + xx) * 16 + (y * 2 + yy) * 4) & 0xFF;
            int expected = sum / 4;
            int got = out[y * 8 + x];
            if (got < expected - 1 || got > expected + 1) ok = false;
        }
    }
    CHECK(ok);
    imgf_free(out);
    imgf_decoder_destroy(d);
}

static void test_resize_decoder_jpeg(void) {
    imgf_buffer_source_t st;
    imgf_err_t err;
    imgf_decoder_t *d = open_jpeg(kJpgGrayBig, sizeof kJpgGrayBig, &st, NULL, &err);
    CHECK(err == IMGF_OK);
    imgf_resize_opts_t o = {0};
    o.target_w = 16; o.target_h = 16; o.fit = IMGF_FIT_STRETCH;
    uint8_t *out = NULL;
    uint16_t ow = 0, oh = 0;
    CHECK(imgf_resize_decoder(d, &o, &out, &ow, &oh) == IMGF_OK);
    CHECK(ow == 16 && oh == 16);
    /* Source is flat 100; resized image should also be ~100. */
    bool flat = true;
    for (int i = 0; i < 16 * 16; i++) {
        int v = out[i];
        if (v < 90 || v > 110) flat = false;
    }
    CHECK(flat);
    imgf_free(out);
    imgf_decoder_destroy(d);
}

/* ---- dither ----------------------------------------------------------- */

static void test_dither_none_index_n16(void) {
    /* gray=128, N=16: level = round(128*15/255) = 8 */
    const uint8_t src[1] = { 128 };
    uint8_t dst[1] = { 0xFF };
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_NONE;
    o.levels = 16;
    o.out_mode = IMGF_DITHER_OUT_INDEX;
    CHECK(imgf_dither_buffer(src, 1, 1, 0, dst, 0, &o) == IMGF_OK);
    CHECK(dst[0] == 8);
}

static void test_dither_none_gray_n16(void) {
    /* gray=128 -> level 8 -> reconstructed = (8*255 + 7) / 15 = 136 */
    const uint8_t src[1] = { 128 };
    uint8_t dst[1] = { 0xFF };
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_NONE;
    o.levels = 16;
    o.out_mode = IMGF_DITHER_OUT_GRAY;
    CHECK(imgf_dither_buffer(src, 1, 1, 0, dst, 0, &o) == IMGF_OK);
    CHECK(dst[0] == 136);
}

static void test_dither_none_n2_threshold(void) {
    /* N=2: gray=127 -> level 0, gray=128 -> level 1 (rounded) */
    const uint8_t src[2] = { 127, 128 };
    uint8_t dst[2] = { 0xFF, 0xFF };
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_NONE;
    o.levels = 2;
    o.out_mode = IMGF_DITHER_OUT_INDEX;
    CHECK(imgf_dither_buffer(src, 2, 1, 0, dst, 0, &o) == IMGF_OK);
    CHECK(dst[0] == 0 && dst[1] == 1);
}

static void test_dither_bayer8_2level_mean(void) {
    /* 8x8 flat 128: Bayer8 with N=2 must produce binary 0/1; mean ≈ half. */
    uint8_t src[64];
    memset(src, 128, sizeof src);
    uint8_t dst[64] = {0};
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_BAYER8;
    o.levels = 2;
    o.out_mode = IMGF_DITHER_OUT_INDEX;
    CHECK(imgf_dither_buffer(src, 8, 8, 0, dst, 0, &o) == IMGF_OK);
    int ones = 0;
    for (int i = 0; i < 64; i++) {
        CHECK(dst[i] == 0 || dst[i] == 1);
        if (dst[i]) ones++;
    }
    /* ~50% set; allow generous slack */
    CHECK(ones >= 24 && ones <= 40);
}

static void test_dither_fs_n16_brackets(void) {
    /* gray=128 + Floyd-Steinberg + N=16: only the bracketing levels 7 and 8
     * survive (matches imgproc test_pipeline_error_diffusion_mean). */
    uint8_t src[32 * 32];
    memset(src, 128, sizeof src);
    uint8_t dst[32 * 32] = {0};
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_FLOYD_STEINBERG;
    o.levels = 16;
    o.serpentine = true;
    o.out_mode = IMGF_DITHER_OUT_INDEX;
    CHECK(imgf_dither_buffer(src, 32, 32, 0, dst, 0, &o) == IMGF_OK);
    bool only_7_8 = true;
    int sum = 0;
    for (int i = 0; i < 32 * 32; i++) {
        if (dst[i] != 7 && dst[i] != 8) only_7_8 = false;
        sum += dst[i];
    }
    CHECK(only_7_8);
    /* mean ~ 128 * 15 / 255 ≈ 7.53; over 1024 pixels ≈ 7700 */
    CHECK(sum >= 7400 && sum <= 8000);
}

static void test_dither_fs_n16_gray_brackets(void) {
    /* Same setup as above but OUT_GRAY: only reconstructed 119 / 136 appear. */
    uint8_t src[16 * 16];
    memset(src, 128, sizeof src);
    uint8_t dst[16 * 16] = {0};
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_FLOYD_STEINBERG;
    o.levels = 16;
    o.serpentine = true;
    o.out_mode = IMGF_DITHER_OUT_GRAY;
    CHECK(imgf_dither_buffer(src, 16, 16, 0, dst, 0, &o) == IMGF_OK);
    bool only_two = true;
    for (int i = 0; i < 16 * 16; i++) {
        if (dst[i] != 119 && dst[i] != 136) only_two = false;
    }
    CHECK(only_two);
}

static void test_dither_buffer_inplace(void) {
    /* In-place dither: dst == src. */
    uint8_t buf[16];
    for (int i = 0; i < 16; i++) buf[i] = (uint8_t)(i * 16);
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_NONE;
    o.levels = 16;
    o.out_mode = IMGF_DITHER_OUT_INDEX;
    CHECK(imgf_dither_buffer(buf, 16, 1, 0, buf, 0, &o) == IMGF_OK);
    /* buf[i] = round(i*16 * 15 / 255) for each gray-step i*16. */
    for (int i = 0; i < 16; i++) {
        int expected = ((i * 16) * 15 * 2 + 255) / 510;
        if (expected > 15) expected = 15;
        CHECK(buf[i] == expected);
    }
}

static void test_dither_push_pop_drain(void) {
    /* Layer 1 direct: feed all rows, drain pops. */
    uint8_t src[2 * 4] = { 0, 50, 100, 200,  20, 70, 120, 220 };
    uint8_t got[2 * 4] = { 0 };
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_NONE;
    o.levels = 4;
    o.out_mode = IMGF_DITHER_OUT_INDEX;
    imgf_err_t err;
    imgf_dither_t *d = imgf_dither_create(4, &o, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_dither_levels(d) == 4);
    CHECK(imgf_dither_out_mode(d) == IMGF_DITHER_OUT_INDEX);
    for (int y = 0; y < 2; y++) {
        CHECK(imgf_dither_push_row(d, src + y * 4) == 1);
        CHECK(imgf_dither_pop_row(d, got + y * 4));
    }
    /* Quantize each by round(v*3/255): 0->0, 50->1, 100->1, 200->2,
       20->0, 70->1, 120->1, 220->3 (round(220*3/255)=round(2.588)=3). */
    CHECK(got[0] == 0 && got[1] == 1 && got[2] == 1 && got[3] == 2);
    CHECK(got[4] == 0 && got[5] == 1 && got[6] == 1 && got[7] == 3);
    imgf_dither_destroy(d);
}

static void test_dither_double_push_rejected(void) {
    /* Two pushes without a pop in between must error. */
    const uint8_t src[1] = { 100 };
    imgf_dither_opts_t o = {0};
    o.algo = IMGF_DITHER_NONE;
    o.levels = 4;
    imgf_err_t err;
    imgf_dither_t *d = imgf_dither_create(1, &o, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_dither_push_row(d, src) == 1);
    CHECK(imgf_dither_push_row(d, src) == -1);
    CHECK(imgf_dither_last_error(d) == IMGF_ERR_INVALID_STATE);
    imgf_dither_destroy(d);
}

static void test_dither_create_invalid(void) {
    imgf_err_t err;
    CHECK(imgf_dither_create(0, NULL, &err) == NULL);
    CHECK(err == IMGF_ERR_INVALID_ARG);
}

/* ---- encoder ---------------------------------------------------------- */

static imgf_err_t raw_encode_buffer(imgf_raw_format_t fmt,
                                     const uint8_t *src, uint16_t w, uint16_t h,
                                     size_t src_stride,
                                     uint8_t *dst, size_t dst_cap, size_t *out_bytes) {
    imgf_err_t err;
    imgf_encoder_t *e = imgf_raw_encoder_create(fmt, w, h, NULL, &err);
    if (!e) return err;
    err = imgf_encoder_encode_buffer(e, src, src_stride, dst, dst_cap, out_bytes);
    imgf_encoder_destroy(e);
    return err;
}

static void test_encoder_format_stride(void) {
    CHECK(imgf_raw_format_stride(IMGF_RAW_L8,     10) == 10);
    CHECK(imgf_raw_format_stride(IMGF_RAW_I4,     10) == 5);
    CHECK(imgf_raw_format_stride(IMGF_RAW_I4,     11) == 6);
    CHECK(imgf_raw_format_stride(IMGF_RAW_I1,     10) == 2);
    CHECK(imgf_raw_format_stride(IMGF_RAW_I1,      7) == 1);
    CHECK(imgf_raw_format_stride(IMGF_RAW_I1,      8) == 1);
    CHECK(imgf_raw_format_stride(IMGF_RAW_I1,      9) == 2);
    CHECK(imgf_raw_format_stride(IMGF_RAW_RGB888,  4) == 12);
    CHECK(imgf_raw_format_stride(IMGF_RAW_RGB565,  4) == 8);
}

static void test_encoder_input_pixfmt(void) {
    CHECK(imgf_raw_format_input_pixfmt(IMGF_RAW_L8)     == IMGF_PIX_GRAY8);
    CHECK(imgf_raw_format_input_pixfmt(IMGF_RAW_I4)     == IMGF_PIX_GRAY8);
    CHECK(imgf_raw_format_input_pixfmt(IMGF_RAW_I1)     == IMGF_PIX_GRAY8);
    CHECK(imgf_raw_format_input_pixfmt(IMGF_RAW_RGB888) == IMGF_PIX_RGB888);
    CHECK(imgf_raw_format_input_pixfmt(IMGF_RAW_RGB565) == IMGF_PIX_RGB565);
}

static void test_encoder_l8_passthrough(void) {
    const uint8_t src[6] = { 10, 20, 30, 40, 50, 60 };
    uint8_t dst[6] = {0};
    size_t written = 0;
    CHECK(raw_encode_buffer(IMGF_RAW_L8, src, 3, 2, 0, dst, sizeof dst, &written) == IMGF_OK);
    CHECK(written == 6);
    CHECK(memcmp(dst, src, 6) == 0);
}

static void test_encoder_i4_pack(void) {
    const uint8_t src[2 * 5] = {
        0xA, 0xB, 0xC, 0xD, 0xE,
        0x1, 0x2, 0x3, 0x4, 0x5,
    };
    uint8_t dst[6] = {0};
    size_t written = 0;
    CHECK(raw_encode_buffer(IMGF_RAW_I4, src, 5, 2, 0, dst, sizeof dst, &written) == IMGF_OK);
    CHECK(written == 6);
    CHECK(dst[0] == 0xAB && dst[1] == 0xCD && dst[2] == 0xE0);
    CHECK(dst[3] == 0x12 && dst[4] == 0x34 && dst[5] == 0x50);
}

static void test_encoder_i1_pack(void) {
    const uint8_t src[2 * 11] = {
        1,0,1,0,1,0,1,0, 1,1,0,
        0,1,0,1,0,1,0,1, 0,0,1,
    };
    uint8_t dst[4] = {0};
    size_t written = 0;
    CHECK(raw_encode_buffer(IMGF_RAW_I1, src, 11, 2, 0, dst, sizeof dst, &written) == IMGF_OK);
    CHECK(written == 4);
    CHECK(dst[0] == 0xAA && dst[1] == 0xC0);
    CHECK(dst[2] == 0x55 && dst[3] == 0x20);
}

static void test_encoder_rgb888_passthrough(void) {
    const uint8_t src[2 * 3] = { 1, 2, 3, 4, 5, 6 };
    uint8_t dst[6] = {0};
    size_t written = 0;
    CHECK(raw_encode_buffer(IMGF_RAW_RGB888, src, 2, 1, 0, dst, sizeof dst, &written) == IMGF_OK);
    CHECK(written == 6);
    CHECK(memcmp(dst, src, 6) == 0);
}

static void test_encoder_rgb565_passthrough(void) {
    const uint16_t src[2] = { 0xF800, 0x07E0 };
    uint8_t dst[4] = {0};
    size_t written = 0;
    CHECK(raw_encode_buffer(IMGF_RAW_RGB565, (const uint8_t *)src, 2, 1, 0,
                            dst, sizeof dst, &written) == IMGF_OK);
    CHECK(written == 4);
    CHECK(memcmp(dst, src, 4) == 0);
}

static void test_encoder_buffer_too_small(void) {
    const uint8_t src[4] = { 1, 2, 3, 4 };
    uint8_t dst[2] = {0};
    CHECK(raw_encode_buffer(IMGF_RAW_L8, src, 4, 1, 0, dst, sizeof dst, NULL)
          == IMGF_ERR_INVALID_ARG);
}

static void test_encoder_push_pop_drain(void) {
    imgf_err_t err;
    imgf_encoder_t *e = imgf_raw_encoder_create(IMGF_RAW_I4, 4, 2, NULL, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_encoder_row_stride(e) == 2);
    CHECK(imgf_encoder_buffer_size(e) == 4);
    CHECK(imgf_encoder_input_pixfmt(e) == IMGF_PIX_GRAY8);
    uint8_t dst[4] = {0};
    CHECK(imgf_encoder_bind_buffer(e, dst, sizeof dst) == IMGF_OK);
    const uint8_t r0[4] = { 0x1, 0x2, 0x3, 0x4 };
    const uint8_t r1[4] = { 0xF, 0xE, 0xD, 0xC };
    CHECK(imgf_encoder_push_row(e, r0) == 1);
    CHECK(imgf_encoder_push_row(e, r1) == 1);
    size_t written = 0;
    CHECK(imgf_encoder_finish(e, &written) == IMGF_OK);
    CHECK(written == 4);
    CHECK(dst[0] == 0x12 && dst[1] == 0x34);
    CHECK(dst[2] == 0xFE && dst[3] == 0xDC);
    imgf_encoder_destroy(e);
}

static void test_encoder_chain_dither_to_i4(void) {
    uint8_t src[16];
    for (int i = 0; i < 16; i++) src[i] = (uint8_t)(i * 17);
    uint8_t indices[16] = {0};
    imgf_dither_opts_t dopts = {0};
    dopts.algo = IMGF_DITHER_FLOYD_STEINBERG;
    dopts.levels = 16;
    dopts.out_mode = IMGF_DITHER_OUT_INDEX;
    CHECK(imgf_dither_buffer(src, 16, 1, 0, indices, 0, &dopts) == IMGF_OK);
    for (int i = 0; i < 16; i++) CHECK(indices[i] == i);
    uint8_t i4[8] = {0};
    size_t written = 0;
    CHECK(raw_encode_buffer(IMGF_RAW_I4, indices, 16, 1, 0, i4, sizeof i4, &written) == IMGF_OK);
    CHECK(written == 8);
    for (int i = 0; i < 8; i++) {
        CHECK(i4[i] == (uint8_t)((i * 2) << 4 | (i * 2 + 1)));
    }
}

/* JPEG-encoder helper: encode `src` (Gray8 or RGB888) into a heap buffer, then
 * decode it with the existing JPEG decoder and copy each decoded row into the
 * out buffer. Returns true on success. */
static bool jpeg_roundtrip_full(const uint8_t *src, uint16_t w, uint16_t h,
                                imgf_pixfmt_t pf, int quality,
                                imgf_jpeg_subsample_t subsample,
                                uint8_t *decoded_out, size_t *enc_bytes) {
    int src_bpp = imgf_pixfmt_bpp(pf);
    size_t jpeg_cap = (size_t)w * h * src_bpp * 4 + 2048;
    uint8_t *jpeg_buf = malloc(jpeg_cap);
    if (!jpeg_buf) return false;
    imgf_jpege_opts_t eopts = {0};
    eopts.quality = quality;
    eopts.subsample = subsample;
    imgf_err_t err;
    imgf_encoder_t *e = imgf_jpege_create(w, h, pf, &eopts, &err);
    if (!e) { free(jpeg_buf); return false; }
    size_t bytes = 0;
    err = imgf_encoder_encode_buffer(e, src, 0, jpeg_buf, jpeg_cap, &bytes);
    imgf_encoder_destroy(e);
    if (err != IMGF_OK || bytes < 4) { free(jpeg_buf); return false; }
    if (jpeg_buf[0] != 0xFF || jpeg_buf[1] != 0xD8) { free(jpeg_buf); return false; }
    if (jpeg_buf[bytes - 2] != 0xFF || jpeg_buf[bytes - 1] != 0xD9) {
        free(jpeg_buf); return false;
    }

    imgf_buffer_source_t st;
    imgf_stream_t s = imgf_stream_from_buffer(&st, jpeg_buf, bytes);
    imgf_decoder_t *d = imgf_jpegd_create();
    if (imgf_decoder_open(d, s, NULL) != IMGF_OK) {
        imgf_decoder_destroy(d); free(jpeg_buf); return false;
    }
    if (imgf_decoder_width(d) != w || imgf_decoder_height(d) != h) {
        imgf_decoder_destroy(d); free(jpeg_buf); return false;
    }
    int out_bpp = imgf_pixfmt_bpp(imgf_decoder_pixfmt(d));
    for (uint16_t y = 0; y < h; y++) {
        if (!imgf_decoder_next_row(d, decoded_out + (size_t)y * w * out_bpp)) {
            imgf_decoder_destroy(d); free(jpeg_buf); return false;
        }
    }
    imgf_decoder_destroy(d);
    if (enc_bytes) *enc_bytes = bytes;
    free(jpeg_buf);
    return true;
}

static bool jpeg_roundtrip(const uint8_t *src, uint16_t w, uint16_t h,
                           imgf_pixfmt_t pf, int quality,
                           uint8_t *decoded_out) {
    return jpeg_roundtrip_full(src, w, h, pf, quality,
                               IMGF_JPEG_SUBSAMPLE_DEFAULT, decoded_out, NULL);
}

static void test_jpege_gray_flat_roundtrip(void) {
    uint8_t src[16 * 16];
    memset(src, 128, sizeof src);
    uint8_t out[16 * 16] = {0};
    CHECK(jpeg_roundtrip(src, 16, 16, IMGF_PIX_GRAY8, 90, out));
    bool flat = true;
    for (int i = 0; i < 16 * 16; i++) if (!near_value(out[i], 128, 5)) flat = false;
    CHECK(flat);
}

static void test_jpege_gray_split_roundtrip(void) {
    uint8_t src[16 * 16];
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++) src[y * 16 + x] = x < 8 ? 40 : 220;
    uint8_t out[16 * 16] = {0};
    CHECK(jpeg_roundtrip(src, 16, 16, IMGF_PIX_GRAY8, 90, out));
    /* Interior columns: left dark, right bright. */
    CHECK(near_value(out[8 * 16 + 2], 40, 25));
    CHECK(near_value(out[8 * 16 + 13], 220, 25));
    CHECK(out[8 * 16 + 13] > out[8 * 16 + 2] + 100);
}

static void test_jpege_rgb_red_roundtrip(void) {
    uint8_t src[16 * 16 * 3];
    for (int i = 0; i < 16 * 16; i++) { src[3*i] = 220; src[3*i+1] = 30; src[3*i+2] = 30; }
    uint8_t out[16 * 16 * 3] = {0};
    CHECK(jpeg_roundtrip(src, 16, 16, IMGF_PIX_RGB888, 90, out));
    /* Center pixel: R dominant, G/B low. */
    int c = (8 * 16 + 8) * 3;
    CHECK(near_value(out[c + 0], 220, 25));
    CHECK(near_value(out[c + 1], 30, 30));
    CHECK(near_value(out[c + 2], 30, 30));
    CHECK(out[c + 0] > out[c + 1] + 100);
}

static void test_jpege_non_multiple_of_8(void) {
    /* Width 17 / Height 9: forces row + column padding paths. */
    uint8_t src[17 * 9];
    for (int y = 0; y < 9; y++)
        for (int x = 0; x < 17; x++) src[y * 17 + x] = (uint8_t)(((x + y) * 13) & 0xFF);
    uint8_t out[17 * 9] = {0};
    CHECK(jpeg_roundtrip(src, 17, 9, IMGF_PIX_GRAY8, 95, out));
    /* Center pixel should be within reasonable JPEG tolerance. */
    int err_sum = 0;
    for (int i = 0; i < 17 * 9; i++) err_sum += abs((int)out[i] - (int)src[i]);
    CHECK(err_sum / (17 * 9) <= 12);   /* mean abs error under 12 levels */
}

static void test_jpege_low_quality_smaller(void) {
    /* High quality and low quality should both work and low-quality should
     * produce fewer bytes. */
    uint8_t src[32 * 32 * 3];
    for (int y = 0; y < 32; y++)
        for (int x = 0; x < 32; x++) {
            src[(y * 32 + x) * 3 + 0] = (uint8_t)(x * 8);
            src[(y * 32 + x) * 3 + 1] = (uint8_t)(y * 8);
            src[(y * 32 + x) * 3 + 2] = (uint8_t)((x + y) * 4);
        }
    uint8_t buf[32 * 32 * 12 + 2048];
    imgf_jpege_opts_t o = {0};
    size_t b_hi = 0, b_lo = 0;
    imgf_err_t err;

    o.quality = 95;
    imgf_encoder_t *e1 = imgf_jpege_create(32, 32, IMGF_PIX_RGB888, &o, &err);
    CHECK(e1 != NULL);
    CHECK(imgf_encoder_encode_buffer(e1, src, 0, buf, sizeof buf, &b_hi) == IMGF_OK);
    imgf_encoder_destroy(e1);

    o.quality = 20;
    imgf_encoder_t *e2 = imgf_jpege_create(32, 32, IMGF_PIX_RGB888, &o, &err);
    CHECK(e2 != NULL);
    CHECK(imgf_encoder_encode_buffer(e2, src, 0, buf, sizeof buf, &b_lo) == IMGF_OK);
    imgf_encoder_destroy(e2);

    CHECK(b_lo < b_hi);
    CHECK(b_lo > 0 && b_hi > 0);
}

static void test_jpege_subsample_roundtrip(void) {
    /* RGB image at each subsampling mode. The decoder handles all three; the
     * round-trip should preserve the dominant color within tolerance. */
    uint8_t src[16 * 16 * 3];
    for (int y = 0; y < 16; y++) {
        for (int x = 0; x < 16; x++) {
            src[(y * 16 + x) * 3 + 0] = (uint8_t)(180 + x);
            src[(y * 16 + x) * 3 + 1] = (uint8_t)(60 + y);
            src[(y * 16 + x) * 3 + 2] = (uint8_t)(40 + ((x + y) >> 1));
        }
    }
    uint8_t out[16 * 16 * 3];

    /* 4:4:4 */
    memset(out, 0, sizeof out);
    size_t sz_444 = 0;
    CHECK(jpeg_roundtrip_full(src, 16, 16, IMGF_PIX_RGB888, 90,
                              IMGF_JPEG_SUBSAMPLE_444, out, &sz_444));
    int center444 = (8 * 16 + 8) * 3;
    CHECK(near_value(out[center444 + 0], 188, 30));
    CHECK(near_value(out[center444 + 1], 68, 30));

    /* 4:2:2 */
    memset(out, 0, sizeof out);
    size_t sz_422 = 0;
    CHECK(jpeg_roundtrip_full(src, 16, 16, IMGF_PIX_RGB888, 90,
                              IMGF_JPEG_SUBSAMPLE_422, out, &sz_422));
    CHECK(near_value(out[center444 + 0], 188, 30));

    /* 4:2:0 */
    memset(out, 0, sizeof out);
    size_t sz_420 = 0;
    CHECK(jpeg_roundtrip_full(src, 16, 16, IMGF_PIX_RGB888, 90,
                              IMGF_JPEG_SUBSAMPLE_420, out, &sz_420));
    CHECK(near_value(out[center444 + 0], 188, 30));

    /* File size: more aggressive subsampling = smaller. */
    CHECK(sz_420 <= sz_422);
    CHECK(sz_422 <= sz_444);
}

static void test_jpege_subsample_padding(void) {
    /* 17x17 image. MCU sizes: 8 (4:4:4), 16x8 (4:2:2), 16x16 (4:2:0).
     * All three modes must handle the boundary padding. */
    uint8_t src[17 * 17 * 3];
    for (int i = 0; i < 17 * 17 * 3; i++) src[i] = (uint8_t)(i & 0xFF);
    uint8_t out[17 * 17 * 3];
    for (int s = IMGF_JPEG_SUBSAMPLE_444; s <= IMGF_JPEG_SUBSAMPLE_420; s++) {
        memset(out, 0, sizeof out);
        CHECK(jpeg_roundtrip_full(src, 17, 17, IMGF_PIX_RGB888, 80,
                                  (imgf_jpeg_subsample_t)s, out, NULL));
    }
}

static void test_jpege_gray_ignores_subsample(void) {
    /* Grayscale must produce identical output regardless of subsample option
     * (it's a single-component image, no chroma to subsample). */
    uint8_t src[16 * 16];
    for (int i = 0; i < 16 * 16; i++) src[i] = (uint8_t)((i * 13) & 0xFF);
    uint8_t out444[16 * 16], out420[16 * 16];
    size_t sz444, sz420;
    CHECK(jpeg_roundtrip_full(src, 16, 16, IMGF_PIX_GRAY8, 80,
                              IMGF_JPEG_SUBSAMPLE_444, out444, &sz444));
    CHECK(jpeg_roundtrip_full(src, 16, 16, IMGF_PIX_GRAY8, 80,
                              IMGF_JPEG_SUBSAMPLE_420, out420, &sz420));
    CHECK(sz444 == sz420);
    CHECK(memcmp(out444, out420, sizeof out444) == 0);
}

static void test_jpege_format_metadata(void) {
    /* Variable-rate JPEG: 0 stride from row_stride accessor on an instance. */
    imgf_err_t err;
    imgf_encoder_t *e = imgf_jpege_create(8, 8, IMGF_PIX_RGB888, NULL, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_encoder_row_stride(e) == 0);
    CHECK(imgf_encoder_input_pixfmt(e) == IMGF_PIX_RGB888);
    CHECK(imgf_encoder_buffer_size(e) > 0);
    imgf_encoder_destroy(e);

    imgf_encoder_t *eg = imgf_jpege_create(8, 8, IMGF_PIX_GRAY8, NULL, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_encoder_input_pixfmt(eg) == IMGF_PIX_GRAY8);
    imgf_encoder_destroy(eg);

    /* RGB565 is not a valid JPEG input. */
    imgf_encoder_t *e2 = imgf_jpege_create(8, 8, IMGF_PIX_RGB565, NULL, &err);
    CHECK(e2 == NULL);
    CHECK(err == IMGF_ERR_UNSUPPORTED);
}

static void test_encoder_finish_before_full_rejected(void) {
    imgf_err_t err;
    imgf_encoder_t *e = imgf_raw_encoder_create(IMGF_RAW_L8, 4, 2, NULL, &err);
    CHECK(err == IMGF_OK);
    uint8_t dst[8] = {0};
    CHECK(imgf_encoder_bind_buffer(e, dst, sizeof dst) == IMGF_OK);
    const uint8_t row[4] = { 1, 2, 3, 4 };
    CHECK(imgf_encoder_push_row(e, row) == 1);   /* only 1 of 2 rows */
    CHECK(imgf_encoder_finish(e, NULL) == IMGF_ERR_INVALID_STATE);
    imgf_encoder_destroy(e);
}

static void test_resizer_push_pop_drain(void) {
    /* Walk the Layer 1 API directly: feed all src rows, drain pops. */
    uint8_t src[16];
    for (int i = 0; i < 16; i++) src[i] = (uint8_t)(i * 10);
    imgf_resize_opts_t o = {0};
    o.target_w = 2; o.target_h = 2; o.fit = IMGF_FIT_STRETCH;
    imgf_err_t err;
    imgf_resizer_t *r = imgf_resizer_create(4, 4, IMGF_PIX_GRAY8, &o, &err);
    CHECK(err == IMGF_OK);
    CHECK(imgf_resizer_dst_width(r) == 2 && imgf_resizer_dst_height(r) == 2);
    uint8_t got[4];
    int out_idx = 0;
    for (int sy = 0; sy < 4; sy++) {
        int ready = imgf_resizer_push_row(r, src + sy * 4);
        for (int i = 0; i < ready; i++) {
            CHECK(imgf_resizer_pop_row(r, got + out_idx * 2));
            out_idx++;
        }
    }
    imgf_resizer_finish(r);
    while (out_idx < 2 && imgf_resizer_pop_row(r, got + out_idx * 2)) out_idx++;
    CHECK(out_idx == 2);
    CHECK(got[0] == 25 && got[1] == 45 && got[2] == 105 && got[3] == 125);
    imgf_resizer_destroy(r);
}

/* ---- async ------------------------------------------------------------ */

typedef struct {
    int n_rows;
    int sent;
} async_prod_ctx_t;

static imgf_err_t async_simple_producer(void *user, imgf_async_ring_t *ring) {
    async_prod_ctx_t *c = (async_prod_ctx_t *)user;
    size_t row_bytes = imgf_async_ring_row_bytes(ring);
    uint8_t buf[64];
    for (int i = 0; i < c->n_rows; i++) {
        for (size_t k = 0; k < row_bytes; k++) buf[k] = (uint8_t)((i * 7 + k) & 0xFF);
        if (imgf_async_ring_put(ring, buf) != IMGF_OK) break;
        c->sent++;
    }
    return IMGF_OK;
}

typedef struct {
    int n_rows;
    int got;
    int ok;
} async_cons_ctx_t;

static imgf_err_t async_simple_consumer(void *user, imgf_async_ring_t *ring) {
    async_cons_ctx_t *c = (async_cons_ctx_t *)user;
    size_t row_bytes = imgf_async_ring_row_bytes(ring);
    uint8_t buf[64];
    int rc;
    c->ok = 1;
    while ((rc = imgf_async_ring_get(ring, buf)) > 0) {
        for (size_t k = 0; k < row_bytes; k++) {
            if (buf[k] != (uint8_t)((c->got * 7 + k) & 0xFF)) c->ok = 0;
        }
        c->got++;
    }
    return rc < 0 ? IMGF_ERR_DECODE : IMGF_OK;
}

static void test_async_basic_throughput(void) {
    imgf_async_opts_t o = {0};
    o.row_bytes = 16;
    o.ring_slots = 3;          /* force backpressure: producer must wait */
    o.producer_core = -1;      /* no pin on device; ignored on host */
    o.consumer_core = -1;
    async_prod_ctx_t prod = { .n_rows = 200, .sent = 0 };
    async_cons_ctx_t cons = { .n_rows = 200, .got = 0, .ok = 0 };
    CHECK(imgf_async_run(&o,
                         async_simple_producer, &prod,
                         async_simple_consumer, &cons) == IMGF_OK);
    CHECK(prod.sent == 200);
    CHECK(cons.got  == 200);
    CHECK(cons.ok   == 1);
}

static imgf_err_t async_aborting_producer(void *user, imgf_async_ring_t *ring) {
    (void)ring;
    *(int *)user = 1;
    return IMGF_ERR_TRUNCATED;
}

static imgf_err_t async_draining_consumer(void *user, imgf_async_ring_t *ring) {
    int *got = (int *)user;
    uint8_t buf[16];
    int rc;
    while ((rc = imgf_async_ring_get(ring, buf)) > 0) (*got)++;
    return rc < 0 ? IMGF_ERR_DECODE : IMGF_OK;
}

static void test_async_producer_error_propagates(void) {
    imgf_async_opts_t o = {0};
    o.row_bytes = 16;
    int prod_ran = 0, cons_got = 0;
    imgf_err_t err = imgf_async_run(&o,
                                    async_aborting_producer, &prod_ran,
                                    async_draining_consumer, &cons_got);
    CHECK(prod_ran == 1);
    /* Producer's IMGF_ERR_TRUNCATED dominates; consumer may end with 0 or via
     * abort (== IMGF_ERR_DECODE from our wrapper). The aggregate must be the
     * producer's error. */
    CHECK(err == IMGF_ERR_TRUNCATED);
}

/* End-to-end async pipeline: decoder + resize on producer; copy-through on
 * consumer. Verifies the ring delivers all resized rows correctly. */
typedef struct {
    imgf_decoder_t *dec;
    imgf_resizer_t *r;
    int             dst_w;
    int             dst_h;
} chain_prod_ctx_t;

static imgf_err_t chain_producer(void *user, imgf_async_ring_t *ring) {
    chain_prod_ctx_t *c = (chain_prod_ctx_t *)user;
    int src_w = imgf_decoder_width(c->dec);
    int src_h = imgf_decoder_height(c->dec);
    uint8_t *src_row = (uint8_t *)malloc(src_w);
    uint8_t *dst_row = (uint8_t *)malloc(c->dst_w);
    if (!src_row || !dst_row) { free(src_row); free(dst_row); return IMGF_ERR_OOM; }
    for (int sy = 0; sy < src_h; sy++) {
        if (!imgf_decoder_next_row(c->dec, src_row)) { free(src_row); free(dst_row); return IMGF_ERR_TRUNCATED; }
        int ready = imgf_resizer_push_row(c->r, src_row);
        for (int i = 0; i < ready; i++) {
            imgf_resizer_pop_row(c->r, dst_row);
            if (imgf_async_ring_put(ring, dst_row) != IMGF_OK) break;
        }
    }
    if (imgf_resizer_finish(c->r) > 0) {
        imgf_resizer_pop_row(c->r, dst_row);
        imgf_async_ring_put(ring, dst_row);
    }
    free(src_row);
    free(dst_row);
    return IMGF_OK;
}

typedef struct {
    uint8_t *out;
    int      dst_w;
    int      dst_h;
    int      got;
} chain_cons_ctx_t;

static imgf_err_t chain_consumer(void *user, imgf_async_ring_t *ring) {
    chain_cons_ctx_t *c = (chain_cons_ctx_t *)user;
    int rc;
    while ((rc = imgf_async_ring_get(ring, c->out + (size_t)c->got * c->dst_w)) > 0) {
        c->got++;
        if (c->got > c->dst_h) return IMGF_ERR_DECODE;
    }
    return rc < 0 ? IMGF_ERR_DECODE : IMGF_OK;
}

static void test_async_decoder_resize_chain(void) {
    imgf_buffer_source_t st;
    imgf_stream_t s = imgf_stream_from_buffer(&st, kPngGrad16, sizeof kPngGrad16);
    imgf_decoder_t *d = imgf_pngd_create();
    CHECK(imgf_decoder_open(d, s, NULL) == IMGF_OK);

    imgf_resize_opts_t ropt = { .target_w = 8, .target_h = 8, .fit = IMGF_FIT_STRETCH };
    imgf_err_t err;
    imgf_resizer_t *r = imgf_resizer_create(16, 16, IMGF_PIX_GRAY8, &ropt, &err);
    CHECK(err == IMGF_OK);

    uint8_t out[8 * 8] = {0};
    chain_prod_ctx_t prod = { d, r, 8, 8 };
    chain_cons_ctx_t cons = { out, 8, 8, 0 };

    imgf_async_opts_t aopt = {0};
    aopt.row_bytes = 8;
    aopt.ring_slots = 4;
    CHECK(imgf_async_run(&aopt,
                         chain_producer, &prod,
                         chain_consumer, &cons) == IMGF_OK);
    CHECK(cons.got == 8);
    /* Compare against the synchronous resize. */
    uint8_t expected[8 * 8] = {0};
    {
        uint8_t full[16 * 16] = {0};
        imgf_buffer_source_t st2;
        imgf_stream_t s2 = imgf_stream_from_buffer(&st2, kPngGrad16, sizeof kPngGrad16);
        imgf_decoder_t *d2 = imgf_pngd_create();
        CHECK(imgf_decoder_open(d2, s2, NULL) == IMGF_OK);
        for (int y = 0; y < 16; y++) imgf_decoder_next_row(d2, full + y * 16);
        imgf_decoder_destroy(d2);
        CHECK(imgf_resize_buffer(full, 16, 16, 16, IMGF_PIX_GRAY8,
                                  expected, 8, &ropt) == IMGF_OK);
    }
    CHECK(memcmp(out, expected, sizeof out) == 0);

    imgf_resizer_destroy(r);
    imgf_decoder_destroy(d);
}

/* ---- main ------------------------------------------------------------- */

int main(void) {
    test_err_strings();

    test_buffer_stream_read();
    test_file_stream();
    test_file_subrange_jpeg();
    test_custom_stream_callback();

    test_sniff();
    test_sniff_factory_dispatch();

    test_destroy_null();
    test_create_then_destroy();
    test_next_row_before_open();
    test_double_open_rejected();

    test_jpeg_gray_flat();
    test_jpeg_gray_split();
    test_jpeg_rgb_red();
    test_jpeg_decode_time_downscale();
    test_jpeg_truncated_header();
    test_jpeg_progressive_unsupported();

    test_png_gray();
    test_png_rgb();
    test_png_palette();
    test_png_rgba_composite();
    test_png_gradient_inflate();
    test_png_truncated_idat();
    test_png_signature_only();

    test_pixfmt_bpp();
    test_resize_compute_dst();
    test_resize_identity_gray();
    test_resize_downscale_gray_2x();
    test_resize_upscale_gray_2x();
    test_resize_rgb888_downscale();
    test_resize_rgb565_roundtrip();
    test_resize_gray_to_rgb888();
    test_resize_rgb888_to_gray();
    test_resize_inherit_pixfmt();
    test_resize_mixed_axis();
    test_resize_buffer_inplace_downscale();
    test_resize_buffer_overlap_upscale_rejected();
    test_resize_decoder_png();
    test_resize_decoder_jpeg();
    test_resizer_push_pop_drain();

    test_async_basic_throughput();
    test_async_producer_error_propagates();
    test_async_decoder_resize_chain();

    test_dither_none_index_n16();
    test_dither_none_gray_n16();
    test_dither_none_n2_threshold();
    test_dither_bayer8_2level_mean();
    test_dither_fs_n16_brackets();
    test_dither_fs_n16_gray_brackets();
    test_dither_buffer_inplace();
    test_dither_push_pop_drain();
    test_dither_double_push_rejected();
    test_dither_create_invalid();

    test_encoder_format_stride();
    test_encoder_input_pixfmt();
    test_encoder_l8_passthrough();
    test_encoder_i4_pack();
    test_encoder_i1_pack();
    test_encoder_rgb888_passthrough();
    test_encoder_rgb565_passthrough();
    test_encoder_buffer_too_small();
    test_encoder_push_pop_drain();
    test_encoder_chain_dither_to_i4();
    test_encoder_finish_before_full_rejected();

    test_jpege_format_metadata();
    test_jpege_gray_flat_roundtrip();
    test_jpege_gray_split_roundtrip();
    test_jpege_rgb_red_roundtrip();
    test_jpege_non_multiple_of_8();
    test_jpege_low_quality_smaller();
    test_jpege_subsample_roundtrip();
    test_jpege_subsample_padding();
    test_jpege_gray_ignores_subsample();

    if (g_failures) {
        fprintf(stderr, "\n%d failure(s)\n", g_failures);
        return 1;
    }
    printf("all tests passed\n");
    return 0;
}
