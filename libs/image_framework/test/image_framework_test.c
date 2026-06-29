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

#include "imgf_decoder.h"
#include "imgf_err.h"
#include "imgf_jpegd.h"
#include "imgf_pngd.h"
#include "imgf_sniff.h"
#include "imgf_stream.h"

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

    if (g_failures) {
        fprintf(stderr, "\n%d failure(s)\n", g_failures);
        return 1;
    }
    printf("all tests passed\n");
    return 0;
}
