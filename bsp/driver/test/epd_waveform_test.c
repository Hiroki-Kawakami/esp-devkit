/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host unit test for the pure waveform scanline core (epd_waveform.h). The device
 * path (i80 bus, power sequence, refresh loop) is not exercised here -- only the
 * SoC-independent logic: the per-frame gray->action table, the dirty-gated 4bpp
 * scanline pack (the one place packing/nibble-order bugs hide), and the FULL pack
 * that drives every pixel by gray.
 *
 * Build + run:  ./run.sh   (in this directory; compiles + runs, no deps)
 */
#include "../epd_waveform.h"
#include <stdio.h>
#include <string.h>

static int g_checks;
#define CHECK(cond) do { g_checks++; if (!(cond)) { \
    fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); return 1; } } while (0)

/* Pack helper for readable expectations: one 2bpp scanline byte (4 px, leftmost
 * pixel in the high pair). */
#define A4(a,b,c,d) ((uint8_t)(((a)<<6)|((b)<<4)|((c)<<2)|(d)))

/* A frame word: gray 0 -> B(1), gray 15 -> W(2), every other gray holds. */
#define FRAME_B0_W15 ((uint32_t)(1u << (0 * 2)) | (uint32_t)(2u << (15 * 2)))

static int test_build_act16(void) {
    uint8_t act[EPD_GRAY_LEVELS];
    epd_build_act16(FRAME_B0_W15, act);
    CHECK(act[0] == 1);    /* black  */
    CHECK(act[15] == 2);   /* white  */
    CHECK(act[5] == 0);    /* hold   */
    CHECK(act[9] == 0);
    return 0;
}

static int test_blit_line(void) {
    uint8_t act[EPD_GRAY_LEVELS];
    epd_build_act16(FRAME_B0_W15, act);

    /* 8 px. Even pixel = high nibble of its byte. Grays:
     * px0=0 px1=15 px2=9 px3=5 px4=0 px5=0 px6=0 px7=15 */
    uint8_t gram[4] = {
        (uint8_t)((0 << 4) | 15),   /* px0,px1 */
        (uint8_t)((9 << 4) | 5),    /* px2,px3 */
        (uint8_t)((0 << 4) | 0),    /* px4,px5 */
        (uint8_t)((0 << 4) | 15),   /* px6,px7 */
    };
    /* Dirty only px0,px1 (bits 0,1) and px7 (bit 7). */
    uint8_t dirty[1] = { (uint8_t)0x83 };
    uint8_t dst[2] = { 0xEE, 0xEE };

    bool driven = epd_blit_line(8, gram, dirty, act, dst);
    CHECK(driven == true);
    CHECK(dst[0] == A4(1, 2, 0, 0));   /* px0=B px1=W, px2/px3 clean hold */
    CHECK(dst[1] == A4(0, 0, 0, 2));   /* px4..px6 clean, px7=W           */

    /* an all-clean byte drives nothing and emits two hold bytes. */
    uint8_t clean[1] = { 0x00 };
    dst[0] = dst[1] = 0xEE;
    driven = epd_blit_line(8, gram, clean, act, dst);
    CHECK(driven == false);
    CHECK(dst[0] == 0);
    CHECK(dst[1] == 0);

    /* a dirty pixel whose gray holds this frame still drives nothing. */
    uint8_t midgray[4] = { (uint8_t)((5 << 4) | 9), 0, 0, 0 };
    uint8_t alldirty[1] = { 0x03 };   /* px0,px1 dirty, both mid-gray (hold) */
    driven = epd_blit_line(8, midgray, alldirty, act, dst);
    CHECK(driven == false);
    CHECK(dst[0] == 0);
    return 0;
}

static int test_blit_line_full(void) {
    uint8_t act[EPD_GRAY_LEVELS];
    epd_build_act16(FRAME_B0_W15, act);

    /* FULL drives every pixel by gray, no dirty gate.
     * px0=0(B) px1=15(W) px2=5(hold) px3=9(hold) */
    uint8_t gram[2] = { (uint8_t)((0 << 4) | 15), (uint8_t)((5 << 4) | 9) };
    uint8_t dst = 0xEE;
    bool driven = epd_blit_line_full(4, gram, act, &dst);
    CHECK(driven == true);
    CHECK(dst == A4(1, 2, 0, 0));
    return 0;
}

int main(void) {
    struct { const char *name; int (*fn)(void); } tests[] = {
        { "build_act16",    test_build_act16 },
        { "blit_line",      test_blit_line },
        { "blit_line_full", test_blit_line_full },
    };
    int n = sizeof(tests) / sizeof(tests[0]), failed = 0;
    for (int i = 0; i < n; i++) {
        int r = tests[i].fn();
        printf("%-24s %s\n", tests[i].name, r ? "FAIL" : "ok");
        failed += r ? 1 : 0;
    }
    printf("\n%d tests, %d checks, %d failed\n", n, g_checks, failed);
    return failed ? 1 : 0;
}
