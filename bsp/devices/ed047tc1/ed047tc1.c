/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ED047TC1 panel descriptor. The framebuffer, waveform engine, differential
 * drive, async refresh task, and i80 bus all live in the epd_ll driver; this
 * file only supplies the panel-specific bits -- geometry, scanline format, the
 * waveform LUTs, and the board's GPIO map -- and hands them to epd_ll_create.
 *
 * Waveform LUT format (epd_waveform_lut.h macros): one F(...)/T(...) row ==
 * one frame; F selects the 2-bit action by the CURRENT (from) gray, T by the
 * TARGET (to) gray (0 = hold, B = drive to black, W = drive to white).
 * Refresh replays every frame top-to-bottom; the frame count is the array
 * length (no terminator). SETTLE frames may be skipped by the engine to
 * unblock a waiting draw; STOP frames always run (the CLEAR tail keeps them
 * so a clear is never cut short).
 */

#include "ed047tc1.h"
#include "epd_ll.h"
#include <stdint.h>
#include <stddef.h>
#include "epd_waveform_lut.h"

/* Panel geometry + scanline format. ED047TC1 is driven at 2 bits/pixel on the
 * 8-bit source bus, so one scanline is width/4 bytes. */
#define ED047TC1_WIDTH        960
#define ED047TC1_HEIGHT       540
#define ED047TC1_PCLK_HZ      20000000
#define ED047TC1_LINE_BYTES   (ED047TC1_WIDTH / 4)   /* 240 */
#define ED047TC1_LINE_PADDING 16

/* 16-grayscale, flashing, highest quality (GC16-class). The flash prelude is
 * from-indexed (F): the drive alternates against the CURRENT gray, so already-
 * white pixels flash in counter-phase instead of being slammed with the rest. */
static const uint32_t ed047tc1_lut_quality[][16] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ F(W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B),
/*  2 */ F(W, W, 0, B, B, B, B, B, W, W, W, W, 0, 0, B, B),
/*  3 */ F(W, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  4 */ F(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, B),
/*  5 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  6 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  7 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  8 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  9 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/* 10 */ T(B, B, B, B, B, B, B, W, 0, W, B, B, B, W, W, B),
/* 11 */ T(W, W, W, W, W, B, B, B, B, B, W, B, 0, B, B, B),
/* 12 */ T(B, B, B, B, B, B, B, B, W, B, B, B, B, B, B, B),
/* 13 */ T(B, W, B, B, B, W, W, B, B, B, B, W, W, B, B, B),
/* 14 */ T(B, B, W, 0, B, B, 0, B, B, B, B, B, B, B, B, W),
/* 15 */ T(B, B, 0, W, W, W, 0, 0, B, B, B, B, B, B, 0, W),
/* 16 */ T(B, B, B, B, B, B, B, B, B, 0, 0, 0, 0, W, W, W),
/* 17 */ T(B, B, B, B, 0, 0, W, W, W, W, W, W, W, W, W, W),
/* 18 */ T(B, B, B, 0, 0, 0, B, 0, 0, 0, 0, 0, 0, 0, 0, W),
    /* settle tail (16 frames, no drive, skippable) */
    SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE,
    SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE,
};

static const uint32_t ed047tc1_lut_text[][16] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ F(W, B, B, B, B, B, B, B, W, W, W, W, W, W, W, B),
/*  2 */ F(W, W, 0, B, B, B, B, B, W, W, W, W, 0, 0, B, B),
/*  3 */ F(W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B),
/*  4 */ F(W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B),
/*  6 */ T(W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B),
/*  7 */ T(W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B),
/*  8 */ T(B, W, W, W, W, W, W, W, B, B, B, B, B, B, B, W),
/*  9 */ T(B, B, 0, W, W, W, W, W, B, B, B, B, 0, 0, W, W),
/* 10 */ T(B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W),
/* 11 */ T(B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W),
/* 12 */ T(B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W),
/* 13 */ T(B, B, B, B, B, B, B, B, W, W, W, W, W, W, W, W),
    /* settle tail (16 frames, no drive, skippable) */
    SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE, SETTLE,
};

/* 2-level fast direct update: only black/white targets drive, mid-grays held. */
static const uint32_t ed047tc1_lut_fast[][16] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B),
/*  2 */ T(W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B),
/*  3 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  4 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  5 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  6 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  7 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  8 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
    /* settle tail (shorter: fast mode trades settle for speed; skippable) */
    SETTLE, SETTLE, SETTLE, SETTLE,
};

/* Drive everything to white. Uniform frames, so the engine reuses one prebuilt
 * scanline for all rows -- a fast way to establish a known white baseline. The
 * tail is STOP, not SETTLE: a clear must never be cut short or the following
 * refresh starts from an unsettled glass. */
static const uint32_t ed047tc1_lut_clear[][16] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ F(W, W, W, W, W, W, W, W, B, B, B, B, B, B, B, B),
/*  2 */ F(W, W, 0, B, B, B, B, B, W, W, W, W, 0, 0, B, B),
/*  3 */ F(W, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  4 */ F(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, B),
/*  5 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  6 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  7 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  8 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  9 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/* 10 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/* 11 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/* 12 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
    /* settle tail (16 frames, no drive, NOT skippable) */
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
};

#define LUT_STEPS(lut) (sizeof(lut) / sizeof((lut)[0]))
_Static_assert(LUT_STEPS(ed047tc1_lut_quality) <= EPD_WF_STEP_MAX, "quality LUT too long");
_Static_assert(LUT_STEPS(ed047tc1_lut_fast)    <= EPD_WF_STEP_MAX, "fast LUT too long");
_Static_assert(LUT_STEPS(ed047tc1_lut_clear)   <= EPD_WF_STEP_MAX, "clear LUT too long");

static epd_ll_lut_t get_waveform_lut(epd_ll_waveform_t waveform, size_t *steps) {
    switch (waveform) {
        case EPD_LL_WAVEFORM_FAST:
            *steps = LUT_STEPS(ed047tc1_lut_fast);
            return ed047tc1_lut_fast;
        case EPD_LL_WAVEFORM_QUALITY:
            *steps = LUT_STEPS(ed047tc1_lut_quality);
            return ed047tc1_lut_quality;
        case EPD_LL_WAVEFORM_TEXT:
            *steps = LUT_STEPS(ed047tc1_lut_text);
            return ed047tc1_lut_text;
        case EPD_LL_WAVEFORM_CLEAR:
            *steps = LUT_STEPS(ed047tc1_lut_clear);
            return ed047tc1_lut_clear;
        default:
            *steps = 0;
            return NULL;
    }
}

esp_err_t ed047tc1_epd_create(const ed047tc1_config_t *cfg, bsp_display_t **out_display) {
    epd_ll_config_t ec = {
        .sph_pin          = cfg->sph_pin,
        .cl_pin           = cfg->cl_pin,
        .ckv_pin          = cfg->ckv_pin,
        .spv_pin          = cfg->spv_pin,
        .le_pin           = cfg->le_pin,
        .oe_pin           = cfg->oe_pin,
        .pwr_pin          = cfg->pwr_pin,
        .dc_dummy_pin     = cfg->pwr_pin,   /* PWR lent as the i80 dummy D/C pin */
        .pclk_hz          = ED047TC1_PCLK_HZ,
        .width            = ED047TC1_WIDTH,
        .height           = ED047TC1_HEIGHT,
        .line_bytes       = ED047TC1_LINE_BYTES,
        .line_padding     = ED047TC1_LINE_PADDING,
        .get_waveform_lut = get_waveform_lut,
        .task_priority    = cfg->task_priority,
        .task_affinity    = cfg->task_affinity,
    };
    for (int i = 0; i < 8; i++) ec.data_pins[i] = cfg->data_pins[i];
    return epd_ll_create(&ec, out_display);
}
