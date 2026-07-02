/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host unit test for the pure per-pixel waveform core (epd_waveform.h) and the
 * LUT authoring macros (epd_waveform_lut.h). The device path (i80 bus, async
 * task, locking) is not exercised here -- only the SoC-independent logic:
 * diff-skip and conflict detection on draw, the per-frame b1 tables, scanline
 * packing, settle-skip, and terminal retire (the one place the design can
 * corrupt -- a stale start frame replaying the wrong step).
 *
 * Build + run:  ./run.sh   (in this directory; compiles + runs, no deps)
 */
#include "../epd_waveform.h"
#include "../epd_waveform_lut.h"
#include <stdio.h>
#include <string.h>

static int g_checks;
#define CHECK(cond) do { g_checks++; if (!(cond)) { \
    fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); return 1; } } while (0)

#define A4(a,b,c,d) ((uint8_t)(((a)<<6)|((b)<<4)|((c)<<2)|(d)))
#define PX(confirmed, target, b1) ((uint16_t)(((b1) << 8) | ((confirmed) << 4) | (target)))

/* 4-frame quality-style LUT: one from-dependent flash frame, one to-dependent
 * drive frame, then a 2-frame skippable settle tail. */
static const uint32_t LUT_Q[][16] = {
    F(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
    T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
    SETTLE,
    SETTLE,
};
#define LUT_Q_STEPS (sizeof(LUT_Q) / sizeof(LUT_Q[0]))

/* clear-style LUT: one uniform drive-to-white frame + a NON-skippable STOP. */
static const uint32_t LUT_C[][16] = {
    T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
    STOP,
};
#define LUT_C_STEPS (sizeof(LUT_C) / sizeof(LUT_C[0]))

static int test_lut_macros(void) {
    /* I packs to-indexed 2-bit actions from bit 0 up. */
    CHECK(I(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W) ==
          ((uint32_t)1 | ((uint32_t)2 << 30)));
    /* One STOP / SETTLE is exactly one frame (the old scalar-flattening trap). */
    CHECK(LUT_C_STEPS == 2);
    CHECK(LUT_Q_STEPS == 4);
    for (int i = 0; i < 16; i++) {
        CHECK(LUT_Q[2][i] == EPD_SETTLE_WORD);   /* SETTLE = marker words */
        CHECK(LUT_C[1][i] == 0);                 /* STOP = real all-hold words */
    }
    /* F: action selected by from; T: action selected by to. */
    CHECK(((LUT_Q[0][0]  >> (15 * 2)) & 3) == B);   /* from 0, any to -> B */
    CHECK(((LUT_Q[0][15] >> (0 * 2))  & 3) == W);   /* from 15, any to -> W */
    CHECK(((LUT_Q[1][7]  >> (0 * 2))  & 3) == B);   /* any from, to 0 -> B */
    CHECK(((LUT_Q[1][7]  >> (15 * 2)) & 3) == W);   /* any from, to 15 -> W */
    CHECK(((LUT_Q[1][7]  >> (5 * 2))  & 3) == 0);
    return 0;
}

static int test_waveform_init(void) {
    epd_waveform_t wf;
    CHECK(epd_waveform_init(&wf, LUT_Q, LUT_Q_STEPS));
    CHECK(wf.steps == 4 && wf.first_skippable == 2);
    CHECK(epd_waveform_init(&wf, LUT_C, LUT_C_STEPS));
    CHECK(wf.steps == 2 && wf.first_skippable == 2);   /* STOP tail: not skippable */

    CHECK(!epd_waveform_init(&wf, LUT_Q, 0));
    CHECK(!epd_waveform_init(&wf, LUT_Q, EPD_WF_STEP_MAX + 1));
    CHECK(!epd_waveform_init(&wf, NULL, 1));

    /* a frame mixing marker and real words is rejected */
    static const uint32_t mixed[][16] = {
        { EPD_SETTLE_WORD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
    };
    CHECK(!epd_waveform_init(&wf, mixed, 1));
    return 0;
}

static int test_draw_px(void) {
    bool conflict, nonidle;
    /* idle showing 9, draw 9 -> diff-skip. */
    uint16_t idle9 = PX(9, 9, EPD_B1_IDLE);
    CHECK(epd_draw_px(idle9, 9, &conflict, &nonidle) == idle9);
    CHECK(!conflict && !nonidle);

    /* idle showing 9, draw 4 -> PENDING(confirmed 9, target 4). */
    CHECK(epd_draw_px(idle9, 4, &conflict, &nonidle) == PX(9, 4, EPD_B1_PENDING));
    CHECK(!conflict && nonidle);

    /* pending, retarget -> keep PENDING, no recount. */
    uint16_t pend = PX(9, 4, EPD_B1_PENDING);
    CHECK(epd_draw_px(pend, 7, &conflict, &nonidle) == PX(9, 7, EPD_B1_PENDING));
    CHECK(!conflict && !nonidle);
    CHECK(epd_draw_px(pend, 4, &conflict, &nonidle) == pend);   /* same target */
    CHECK(!conflict && !nonidle);

    /* active toward 4: redraw 4 -> unchanged (finish it); draw 12 -> CONFLICT,
     * unchanged (never interrupt). */
    uint16_t act = PX(9, 4, epd_b1_armed(0, 10));
    CHECK(epd_px_is_active(act));
    CHECK(epd_draw_px(act, 4, &conflict, &nonidle) == act);
    CHECK(!conflict && !nonidle);
    CHECK(epd_draw_px(act, 12, &conflict, &nonidle) == act);
    CHECK(conflict && !nonidle);

    CHECK(!epd_px_is_active(idle9));
    CHECK(!epd_px_is_active(pend));
    return 0;
}

static int test_frame_tab(void) {
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));
    CHECK(epd_waveform_init(&wf[1], LUT_C, LUT_C_STEPS));

    const uint32_t *rowset[256];
    uint8_t retire[256];

    /* engine at frame 10; pixels started at 10 are at step 0, at 9 -> step 1... */
    epd_build_frame_tab(wf, 10, false, rowset, retire);

    CHECK(rowset[EPD_B1_IDLE] == epd_hold_rowset && !retire[EPD_B1_IDLE]);
    CHECK(rowset[EPD_B1_PENDING] == epd_hold_rowset && !retire[EPD_B1_PENDING]);

    uint8_t armed = epd_b1_armed(0, 10);        /* start 11: step 63 this frame */
    CHECK(rowset[armed] == epd_hold_rowset && !retire[armed]);

    CHECK(rowset[(0 << 6) | 10] == LUT_Q[0]);   /* step 0: from-flash frame */
    CHECK(!retire[(0 << 6) | 10]);
    CHECK(rowset[(0 << 6) | 9] == LUT_Q[1]);    /* step 1 */
    CHECK(!retire[(0 << 6) | 9]);
    CHECK(rowset[(0 << 6) | 8] == epd_hold_rowset);   /* step 2: settle -> hold */
    CHECK(!retire[(0 << 6) | 8]);               /* no skip: keeps settling */
    CHECK(rowset[(0 << 6) | 7] == epd_hold_rowset);   /* step 3: last frame */
    CHECK(retire[(0 << 6) | 7]);

    /* skip_settle: everything at/entering the skippable tail retires. */
    epd_build_frame_tab(wf, 10, true, rowset, retire);
    CHECK(!retire[(0 << 6) | 10]);              /* step 0 still drives */
    CHECK(retire[(0 << 6) | 9]);                /* step 1: remaining all settle */
    CHECK(retire[(0 << 6) | 8]);
    /* ...but the clear LUT's STOP tail is not skippable. */
    CHECK(!retire[(1 << 6) | 10]);              /* clear step 0 */
    CHECK(rowset[(1 << 6) | 9] == LUT_C[1]);    /* STOP renders as a real frame */
    CHECK(retire[(1 << 6) | 9]);                /* last step retires normally */

    /* unbound slot 2: self-heals to retire. */
    CHECK(retire[(2 << 6) | 10] && rowset[(2 << 6) | 10] == epd_hold_rowset);
    return 0;
}

static int test_blit_line(void) {
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));
    const uint32_t *rowset[256];
    uint8_t retire[256];
    epd_build_frame_tab(wf, 0, false, rowset, retire);

    uint8_t s0 = (0 << 6) | 0;   /* step 0 (from-flash) */
    uint8_t s1 = (0 << 6) | 63;  /* step 1 (to-drive)   */
    uint16_t row[8] = {
        PX(0, 15, s0),           /* step0 from 0 -> B (flash by from)      */
        PX(15, 0, s0),           /* step0 from 15 -> W                     */
        PX(15, 0, s1),           /* step1 to 0 -> B                        */
        PX(0, 15, s1),           /* step1 to 15 -> W                       */
        PX(5, 9, s0),            /* step0 from 5 -> hold                   */
        PX(9, 4, EPD_B1_PENDING),/* pending -> hold                        */
        PX(9, 9, EPD_B1_IDLE),   /* idle -> hold                           */
        PX(0, 15, epd_b1_armed(0, 0)),   /* armed -> hold this frame       */
    };
    uint8_t dst[2] = {0xEE, 0xEE};
    CHECK(epd_blit_line(8, row, rowset, dst));
    CHECK(dst[0] == A4(B, W, B, W));
    CHECK(dst[1] == A4(0, 0, 0, 0));

    uint16_t idle[4] = { PX(9,9,0xFF), PX(1,1,0xFF), PX(15,15,0xFF), PX(3,3,0xFF) };
    CHECK(!epd_blit_line(4, idle, rowset, dst));
    CHECK(dst[0] == 0);
    return 0;
}

/* The byte-table blit must be bit-identical to the word/shift blit: same
 * lookup, precomputed by epd_build_act256. Fuzz across frames and pixel words. */
static int test_blit_line_tab(void) {
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));
    CHECK(epd_waveform_init(&wf[1], LUT_C, LUT_C_STEPS));

    uint8_t t[256];
    epd_build_act256(LUT_Q[0], t);              /* from-flash frame */
    CHECK(t[(0 << 4) | 15] == B);               /* from 0, any to -> B */
    CHECK(t[(15 << 4) | 0] == W);
    CHECK(t[(5 << 4) | 9] == 0);
    epd_build_act256(LUT_Q[1], t);              /* to-drive frame */
    CHECK(t[(7 << 4) | 0] == B && t[(7 << 4) | 15] == W);

    const uint32_t *rowset[256];
    uint8_t retire[256];
    static uint8_t hold256[256];                /* all zero */
    static uint8_t pool[16][256];
    const uint8_t *tabs[256];

    uint32_t seed = 12345;
    for (uint8_t frame = 0; frame < 12; frame++) {
        epd_build_frame_tab(wf, frame, false, rowset, retire);
        const uint32_t *seen[16];
        int used = 0;
        for (int b1 = 0; b1 < 256; b1++) {
            if (rowset[b1] == epd_hold_rowset) { tabs[b1] = hold256; continue; }
            int j;
            for (j = 0; j < used; j++) {
                if (seen[j] == rowset[b1]) break;
            }
            if (j == used) {
                CHECK(used < 16);
                seen[used] = rowset[b1];
                epd_build_act256(rowset[b1], pool[used]);
                used++;
            }
            tabs[b1] = pool[j];
        }

        enum { W_ = 64 };
        uint16_t row[W_];
        for (int i = 0; i < W_; i++) {
            seed = seed * 1103515245u + 12345u;
            row[i] = (uint16_t)(seed >> 8);
        }
        uint8_t d1[W_ / 4], d2[W_ / 4];
        bool r1 = epd_blit_line(W_, row, rowset, d1);
        bool r2 = epd_blit_line_tab(W_, row, tabs, d2);
        CHECK(r1 == r2);
        CHECK(memcmp(d1, d2, sizeof(d1)) == 0);
    }
    return 0;
}

static int test_retire_row(void) {
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));
    const uint32_t *rowset[256];
    uint8_t retire[256];
    /* frame 3: pixels started at 0 are at step 3 (last) -> retire. */
    epd_build_frame_tab(wf, 3, false, rowset, retire);

    uint8_t ending = (0 << 6) | 0, running = (0 << 6) | 2;
    uint16_t row[4] = {
        PX(9, 9, EPD_B1_IDLE), PX(15, 4, ending), PX(15, 2, running), PX(15, 7, ending),
    };
    CHECK(epd_retire_row(4, row, retire) == 2);
    CHECK(row[1] == PX(4, 4, EPD_B1_IDLE));    /* confirmed = target, idle */
    CHECK(row[3] == PX(7, 7, EPD_B1_IDLE));
    CHECK(row[2] == PX(15, 2, running));       /* mid-flight untouched */
    CHECK(row[0] == PX(9, 9, EPD_B1_IDLE));
    return 0;
}

/* End-to-end: draw -> arm -> frame loop across the 6-bit frame-counter wrap,
 * asserting the full waveform replays (no skipped first step, no stale-step
 * corruption) and the terminal retire confirms the target. */
static int test_lifecycle_wrap(void) {
    enum { W_ = 8 };
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));

    uint16_t state[W_];
    for (int i = 0; i < W_; i++) state[i] = PX(15, 15, EPD_B1_IDLE);
    int row_nonidle = 0;

    /* draw: left 4 px to black. */
    bool conflict, ni;
    for (int x = 0; x < 4; x++) {
        state[x] = epd_draw_px(state[x], 0, &conflict, &ni);
        CHECK(!conflict);
        if (ni) row_nonidle++;
    }
    CHECK(row_nonidle == 4);

    /* refresh at engine frame 61 -> start 62, straddling the mod-64 wrap. */
    unsigned frame = 61;
    uint8_t b1 = epd_b1_armed(0, frame);
    CHECK(b1 == ((0 << 6) | 62));
    for (int x = 0; x < 4; x++) state[x] = (uint16_t)((state[x] & 0xFF) | (b1 << 8));

    const uint32_t *rowset[256];
    uint8_t retire[256];
    uint8_t dst[W_ / 4];
    int frames_driven = 0;
    for (int f = 0; f < 10 && row_nonidle > 0; f++) {
        frame = (frame + 1) & 63;
        epd_build_frame_tab(wf, (uint8_t)frame, false, rowset, retire);
        bool driven = epd_blit_line(W_, state, rowset, dst);
        if (frame == 62) {   /* step 0: from 15 -> W flash */
            CHECK(driven);
            CHECK(dst[0] == A4(W, W, W, W) && dst[1] == 0);
        }
        if (frame == 63) {   /* step 1: to 0 -> B */
            CHECK(driven);
            CHECK(dst[0] == A4(B, B, B, B) && dst[1] == 0);
        }
        if (frame == 0 || frame == 1) CHECK(!driven);   /* settle */
        if (driven) frames_driven++;
        row_nonidle -= epd_retire_row(W_, state, retire);
    }
    CHECK(frames_driven == 2);
    CHECK(row_nonidle == 0);                      /* retired on step 3, frame 1 */
    for (int x = 0; x < 4; x++) CHECK(state[x] == PX(0, 0, EPD_B1_IDLE));
    for (int x = 4; x < W_; x++) CHECK(state[x] == PX(15, 15, EPD_B1_IDLE));

    /* redraw black -> diff-skip against the new confirmed value. */
    CHECK(epd_draw_px(state[0], 0, &conflict, &ni) == state[0]);
    CHECK(!ni);
    return 0;
}

/* Two pixels armed on different frames progress independently -- the per-pixel
 * replacement for the old 15-generation pipeline (no slot limit). */
static int test_concurrent_generations(void) {
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));

    uint16_t state[4] = {
        PX(15, 0, (0 << 6) | 1),          /* armed at frame 0 -> step = f-1   */
        PX(15, 15, EPD_B1_IDLE),
        PX(15, 15, EPD_B1_IDLE),
        PX(15, 15, EPD_B1_IDLE),
    };
    const uint32_t *rowset[256];
    uint8_t retire[256];
    uint8_t dst;

    /* frame 1: A at step 0. Arm B mid-scan (starts frame 2). */
    epd_build_frame_tab(wf, 1, false, rowset, retire);
    CHECK(epd_blit_line(4, state, rowset, &dst));
    CHECK(dst == A4(W, 0, 0, 0));                 /* A: from 15 flash W */
    state[2] = PX(15, 0, epd_b1_armed(0, 1));     /* B armed during frame 1 */
    CHECK(epd_retire_row(4, state, retire) == 0); /* nobody retires, B safe */

    /* frame 2: A step 1 (to 0 -> B), B step 0 (from 15 -> W). */
    epd_build_frame_tab(wf, 2, false, rowset, retire);
    CHECK(epd_blit_line(4, state, rowset, &dst));
    CHECK(dst == A4(B, 0, W, 0));
    epd_retire_row(4, state, retire);

    /* frames 3,4: A settles then retires; B follows one frame behind. */
    epd_build_frame_tab(wf, 3, false, rowset, retire);
    CHECK(!epd_blit_line(4, state, rowset, &dst) || dst == A4(0, 0, B, 0));
    epd_retire_row(4, state, retire);
    epd_build_frame_tab(wf, 4, false, rowset, retire);
    epd_retire_row(4, state, retire);
    CHECK(state[0] == PX(0, 0, EPD_B1_IDLE));     /* A done (step 3 at frame 4) */
    CHECK(epd_px_is_active(state[2]));            /* B still settling */
    epd_build_frame_tab(wf, 5, false, rowset, retire);
    epd_retire_row(4, state, retire);
    CHECK(state[2] == PX(0, 0, EPD_B1_IDLE));     /* B done one frame later */
    return 0;
}

/* settle-skip end-to-end: with a waiter, pixels retire as soon as only the
 * skippable tail remains -- but a clear-style STOP tail runs to completion. */
static int test_settle_skip(void) {
    epd_waveform_t wf[EPD_WF_SLOTS] = {0};
    CHECK(epd_waveform_init(&wf[0], LUT_Q, LUT_Q_STEPS));
    CHECK(epd_waveform_init(&wf[1], LUT_C, LUT_C_STEPS));

    uint16_t state[4] = {
        PX(15, 0, (0 << 6) | 1),   /* quality, starts frame 1 */
        PX(15, 15, (1 << 6) | 1),  /* clear,   starts frame 1 */
        PX(15, 15, EPD_B1_IDLE),
        PX(15, 15, EPD_B1_IDLE),
    };
    const uint32_t *rowset[256];
    uint8_t retire[256];

    /* frame 1 (steps 0): drives, nothing retires even with skip. */
    epd_build_frame_tab(wf, 1, true, rowset, retire);
    CHECK(epd_retire_row(4, state, retire) == 0);
    /* frame 2 (step 1): quality's remaining frames are all SETTLE -> retires
     * under skip; clear's step 1 is its STOP (last step) -> retires normally,
     * having run every frame. */
    epd_build_frame_tab(wf, 2, true, rowset, retire);
    CHECK(epd_retire_row(4, state, retire) == 2);
    CHECK(state[0] == PX(0, 0, EPD_B1_IDLE));
    CHECK(state[1] == PX(15, 15, EPD_B1_IDLE));

    /* without skip, quality at step 1 does NOT retire. */
    uint16_t s2[1] = { PX(15, 0, (0 << 6) | 1) };
    epd_build_frame_tab(wf, 2, false, rowset, retire);
    CHECK(epd_retire_row(1, s2, retire) == 0);
    return 0;
}

static int test_rowset_uniform(void) {
    uint8_t byte;
    CHECK(epd_rowset_uniform(LUT_C[0], &byte));   /* uniform drive-to-white */
    CHECK(byte == 0xAA);
    CHECK(epd_rowset_uniform(LUT_C[1], &byte));   /* STOP: uniform hold */
    CHECK(byte == 0x00);
    CHECK(epd_rowset_uniform(epd_hold_rowset, &byte) && byte == 0x00);
    CHECK(!epd_rowset_uniform(LUT_Q[0], &byte));  /* from-dependent flash */
    CHECK(!epd_rowset_uniform(LUT_Q[1], &byte));  /* to-dependent drive */
    return 0;
}

int main(void) {
    struct { const char *name; int (*fn)(void); } tests[] = {
        { "lut_macros",             test_lut_macros },
        { "waveform_init",          test_waveform_init },
        { "draw_px",                test_draw_px },
        { "frame_tab",              test_frame_tab },
        { "blit_line",              test_blit_line },
        { "blit_line_tab",          test_blit_line_tab },
        { "retire_row",             test_retire_row },
        { "lifecycle_wrap",         test_lifecycle_wrap },
        { "concurrent_generations", test_concurrent_generations },
        { "settle_skip",            test_settle_skip },
        { "rowset_uniform",         test_rowset_uniform },
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
