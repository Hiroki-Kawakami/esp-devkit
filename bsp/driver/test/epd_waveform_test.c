/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host unit test for the pure transaction-index waveform core (epd_waveform.h).
 * The device path (i80 bus, async task, locking) is not exercised here -- only
 * the SoC-independent logic: diff-skip on draw, slot allocation, the per-frame
 * action table, scanline packing, and terminal generation reclaim (the one place
 * the design can corrupt -- a reused id leaking old pixels into a new waveform).
 *
 * Build + run:  ./run.sh   (in this directory; compiles + runs, no deps)
 */
#include "../epd_waveform.h"
#include <stdio.h>
#include <string.h>
#include <assert.h>

static int g_checks;
#define CHECK(cond) do { g_checks++; if (!(cond)) { \
    fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); return 1; } } while (0)

/* Pack helpers for readable expectations: a frame word and a scanline byte. */
#define A4(a,b,c,d) ((uint8_t)(((a)<<6)|((b)<<4)|((c)<<2)|(d)))

/* A 2-frame LUT: frame 0 drives gray 0 -> black and gray 15 -> white, frame 1
 * holds everything (settle). Mid-grays always hold. */
static const uint32_t LUT2[] = {
    /* gray 0 = B(1), gray 15 = W(2), rest hold */
    (uint32_t)(1u << (0 * 2)) | (uint32_t)(2u << (15 * 2)),
    0u,
};

static int test_draw_diffskip(void) {
    bool nonidle;
    /* idle pixel already showing gray 9, draw 9 again -> skip, stays idle. */
    uint8_t b = (9 << 4) | 0;
    CHECK(epd_draw_pixel(b, 9, 3, &nonidle) == b);
    CHECK(nonidle == false);

    /* idle pixel showing 9, draw 4 -> stamp (4, id 3), counts as new non-idle. */
    CHECK(epd_draw_pixel(b, 4, 3, &nonidle) == ((4 << 4) | 3));
    CHECK(nonidle == true);

    /* same open generation (id 3), redraw to 7 -> keep id 3, no recount. */
    uint8_t s = (4 << 4) | 3;
    CHECK(epd_draw_pixel(s, 7, 3, &nonidle) == ((7 << 4) | 3));
    CHECK(nonidle == false);

    /* in flight on a different generation (id 5), draw a DIFFERENT gray under open
     * id 3 -> interrupt onto id 3, no recount (was already non-idle). */
    uint8_t f = (2 << 4) | 5;
    CHECK(epd_draw_pixel(f, 12, 3, &nonidle) == ((12 << 4) | 3));
    CHECK(nonidle == false);

    /* in flight on a different generation, redraw the SAME target gray -> skip,
     * let that generation finish (no restart, no re-stamp). */
    CHECK(epd_draw_pixel(f, 2, 3, &nonidle) == f);
    CHECK(nonidle == false);
    return 0;
}

static int test_alloc_exhaust(void) {
    epd_tx_t tx[EPD_TX_SLOTS] = {0};
    for (int i = 1; i < EPD_TX_SLOTS; i++) {
        int id = epd_tx_alloc(tx);
        CHECK(id == i);
        CHECK(tx[id].state == EPD_TX_PENDING);
    }
    CHECK(epd_tx_alloc(tx) == 0);             /* 15 used -> exhausted */
    tx[7].state = EPD_TX_FREE;
    CHECK(epd_tx_alloc(tx) == 7);             /* freed slot is reusable */
    return 0;
}

static int test_act_tab(void) {
    epd_tx_t tx[EPD_TX_SLOTS] = {0};
    tx[3] = (epd_tx_t){ .lut = LUT2, .steps = 2, .frame = 0, .state = EPD_TX_ACTIVE };
    tx[4] = (epd_tx_t){ .lut = LUT2, .steps = 2, .frame = 0, .state = EPD_TX_PENDING };

    uint8_t act[256];
    epd_build_act_tab(tx, act);
    /* ACTIVE id 3, gray 0 -> black(1); gray 15 -> white(2); gray 5 -> hold(0). */
    CHECK(act[(0 << 4) | 3] == 1);
    CHECK(act[(15 << 4) | 3] == 2);
    CHECK(act[(5 << 4) | 3] == 0);
    /* PENDING id 4 and idle id 0 always hold, regardless of gray. */
    CHECK(act[(0 << 4) | 4] == 0);
    CHECK(act[(15 << 4) | 4] == 0);
    CHECK(act[(0 << 4) | 0] == 0);
    return 0;
}

static int test_blit_line(void) {
    epd_tx_t tx[EPD_TX_SLOTS] = {0};
    tx[1] = (epd_tx_t){ .lut = LUT2, .steps = 2, .frame = 0, .state = EPD_TX_ACTIVE };
    uint8_t act[256];
    epd_build_act_tab(tx, act);

    /* 8 px: black-target/white-target/idle/mid all on id 1, then 4 idle. */
    uint8_t row[8] = {
        (0 << 4) | 1, (15 << 4) | 1, (9 << 4) | 0, (5 << 4) | 1,
        (9 << 4) | 0, (9 << 4) | 0, (9 << 4) | 0, (9 << 4) | 0,
    };
    uint8_t dst[2] = {0xEE, 0xEE};
    bool driven = epd_blit_line(8, row, act, dst);
    CHECK(driven == true);
    CHECK(dst[0] == A4(1, 2, 0, 0));   /* B, W, idle hold, mid hold */
    CHECK(dst[1] == A4(0, 0, 0, 0));   /* all idle */

    /* an all-idle row drives nothing. */
    uint8_t idle[4] = { (9 << 4), (1 << 4), (15 << 4), (3 << 4) };
    driven = epd_blit_line(4, idle, act, dst);
    CHECK(driven == false);
    CHECK(dst[0] == 0);
    return 0;
}

static int test_blit_full(void) {
    /* FULL drives every pixel by gray, ignoring ids: frame 0 -> gray0=B, gray15=W. */
    uint8_t row[4] = { (0 << 4) | 7, (15 << 4) | 0, (5 << 4) | 2, (9 << 4) | 0 };
    uint8_t dst;
    bool driven = epd_blit_line_full(4, row, LUT2[0], &dst);
    CHECK(driven == true);
    CHECK(dst == A4(1, 2, 0, 0));
    return 0;
}

static int test_reclaim(void) {
    /* row with ids 0,3,5,3; reclaim only id 3 -> those go idle, gray kept. */
    uint8_t row[4] = { (9 << 4) | 0, (4 << 4) | 3, (2 << 4) | 5, (7 << 4) | 3 };
    int freed = epd_reclaim_row(4, row, (uint16_t)(1u << 3));
    CHECK(freed == 2);
    CHECK(row[1] == (4 << 4));          /* id cleared, gray 4 kept */
    CHECK(row[3] == (7 << 4));
    CHECK(row[2] == ((2 << 4) | 5));    /* id 5 untouched */
    CHECK(row[0] == (9 << 4));          /* already idle */
    return 0;
}

/* End-to-end: drive one generation to completion through the frame loop the way
 * epd_ll.c will, and assert the terminal reclaim returns the pixel to idle with
 * the target gray confirmed -- so reusing the id later cannot leak old pixels. */
static int test_generation_lifecycle(void) {
    enum { W = 8 };
    uint8_t state[W];
    for (int i = 0; i < W; i++) state[i] = (15 << 4);   /* all white, idle */
    int row_nonidle = 0;

    /* draw: set the left 4 px to black (gray 0) under a freshly allocated gen. */
    epd_tx_t tx[EPD_TX_SLOTS] = {0};
    int id = epd_tx_alloc(tx);
    CHECK(id == 1);
    for (int x = 0; x < 4; x++) {
        bool ni; state[x] = epd_draw_pixel(state[x], 0, id, &ni); if (ni) row_nonidle++;
    }
    CHECK(row_nonidle == 4);

    /* refresh: bind the LUT, activate. */
    tx[id].lut = LUT2; tx[id].steps = 2; tx[id].frame = 0; tx[id].state = EPD_TX_ACTIVE;

    /* run the frame loop until no slot is ACTIVE. */
    int frames = 0;
    for (;;) {
        uint16_t active_mask, ended;
        if (!epd_frame_mark(tx, &active_mask, &ended)) break;
        uint8_t act[256]; epd_build_act_tab(tx, act);
        uint8_t dst[W / 4];
        bool driven = epd_blit_line(W, state, act, dst);
        if (frames == 0) { CHECK(driven); CHECK(dst[0] == A4(1, 1, 1, 1)); CHECK(dst[1] == 0); }
        if (ended) row_nonidle -= epd_reclaim_row(W, state, ended);
        epd_frame_advance(tx, active_mask);
        frames++;
    }
    CHECK(frames == 2);                 /* LUT2 has 2 frames */
    CHECK(row_nonidle == 0);            /* every pixel reclaimed to idle */
    for (int x = 0; x < 4; x++) CHECK(state[x] == (0 << 4));   /* black confirmed, id 0 */
    for (int x = 4; x < W; x++) CHECK(state[x] == (15 << 4));  /* untouched white */

    /* slot 1 is FREE again and the next alloc reuses it -- a fresh draw over the
     * now-black pixels must not inherit anything from the finished generation. */
    CHECK(tx[1].state == EPD_TX_FREE);
    int id2 = epd_tx_alloc(tx);
    CHECK(id2 == 1);
    bool ni;
    /* the reclaimed pixel reads as idle gray 0; drawing white diffs and stamps. */
    uint8_t b = epd_draw_pixel(state[0], 15, id2, &ni);
    CHECK(b == ((15 << 4) | 1));
    CHECK(ni == true);
    /* drawing black again (its confirmed value) diff-skips. */
    CHECK(epd_draw_pixel(state[0], 0, id2, &ni) == state[0]);
    CHECK(ni == false);
    return 0;
}

/* Two non-overlapping generations of different lengths run concurrently and each
 * reclaims independently -- the pipelining Phase 6 exists for. */
static int test_concurrent_generations(void) {
    /* gen A: 1-frame LUT (drive gray0->B then done). gen B: LUT2 (2 frames). */
    static const uint32_t LUT1[] = { (uint32_t)(1u << 0) };
    enum { W = 8 };
    uint8_t state[W];
    for (int i = 0; i < W; i++) state[i] = (15 << 4);

    epd_tx_t tx[EPD_TX_SLOTS] = {0};
    int a = epd_tx_alloc(tx), b = epd_tx_alloc(tx);
    CHECK(a == 1 && b == 2);
    bool ni;
    state[0] = epd_draw_pixel(state[0], 0, a, &ni);   /* left pixel -> gen A */
    state[4] = epd_draw_pixel(state[4], 0, b, &ni);   /* right pixel -> gen B */
    tx[a] = (epd_tx_t){ .lut = LUT1, .steps = 1, .state = EPD_TX_ACTIVE };
    tx[b] = (epd_tx_t){ .lut = LUT2, .steps = 2, .state = EPD_TX_ACTIVE };

    /* frame 0: both drive their pixel to black; A ends here, B continues. */
    uint16_t am, ended; CHECK(epd_frame_mark(tx, &am, &ended));
    CHECK(am == (uint16_t)((1u << a) | (1u << b)));
    CHECK(ended == (uint16_t)(1u << a));          /* only A ends on frame 0 */
    uint8_t act[256]; epd_build_act_tab(tx, act);
    uint8_t dst[W / 4];
    CHECK(epd_blit_line(W, state, act, dst));
    CHECK(dst[0] == A4(1, 0, 0, 0));              /* state[0] (gen A) drives black */
    CHECK(dst[1] == A4(1, 0, 0, 0));              /* state[4] (gen B) drives black */
    epd_reclaim_row(W, state, ended);
    CHECK(state[0] == (0 << 4));                  /* A reclaimed */
    CHECK(state[4] == ((0 << 4) | b));            /* B still in flight */
    epd_frame_advance(tx, am);
    CHECK(tx[a].state == EPD_TX_FREE && tx[b].state == EPD_TX_ACTIVE);

    /* frame 1: only B active, settle frame, then B ends. */
    CHECK(epd_frame_mark(tx, &am, &ended));
    CHECK(ended == (uint16_t)(1u << b));
    epd_build_act_tab(tx, act);
    epd_reclaim_row(W, state, ended);
    epd_frame_advance(tx, am);
    CHECK(state[4] == (0 << 4));
    CHECK(tx[b].state == EPD_TX_FREE);

    /* engine now idle. */
    CHECK(!epd_frame_mark(tx, &am, &ended));
    return 0;
}

/* A generation activated AFTER epd_frame_mark (i.e. during the lock-free scan)
 * must not be advanced before it has been rendered -- it stays at frame 0 so the
 * next frame replays its first frame, not its second. This is the mid-scan
 * activation race. */
static int test_midscan_activation(void) {
    epd_tx_t tx[EPD_TX_SLOTS] = {0};
    tx[1] = (epd_tx_t){ .lut = LUT2, .steps = 2, .frame = 0, .state = EPD_TX_ACTIVE };

    /* frame for gen 1; gen 2 is not active yet at mark time. */
    uint16_t am, ended;
    CHECK(epd_frame_mark(tx, &am, &ended));
    CHECK(am == (uint16_t)(1u << 1));             /* only gen 1 rendered this frame */

    /* ...scan happens here... gen 2 activates mid-scan. */
    tx[2] = (epd_tx_t){ .lut = LUT2, .steps = 2, .frame = 0, .state = EPD_TX_ACTIVE };

    epd_frame_advance(tx, am);                    /* must advance only gen 1 */
    CHECK(tx[1].frame == 1);                      /* gen 1 stepped */
    CHECK(tx[2].frame == 0);                      /* gen 2 NOT skipped past frame 0 */
    CHECK(tx[2].state == EPD_TX_ACTIVE);

    /* next frame renders both, gen 2 from its frame 0. */
    CHECK(epd_frame_mark(tx, &am, &ended));
    CHECK(am == (uint16_t)((1u << 1) | (1u << 2)));
    return 0;
}

int main(void) {
    struct { const char *name; int (*fn)(void); } tests[] = {
        { "draw_diffskip",          test_draw_diffskip },
        { "alloc_exhaust",          test_alloc_exhaust },
        { "act_tab",                test_act_tab },
        { "blit_line",              test_blit_line },
        { "blit_full",              test_blit_full },
        { "reclaim",                test_reclaim },
        { "generation_lifecycle",   test_generation_lifecycle },
        { "concurrent_generations", test_concurrent_generations },
        { "midscan_activation",     test_midscan_activation },
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
