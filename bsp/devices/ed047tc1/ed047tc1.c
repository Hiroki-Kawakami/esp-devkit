/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ED047TC1 panel descriptor. The framebuffer, waveform engine, differential
 * drive, async refresh task, and i80 bus all live in the epd_ll driver; this
 * file only supplies the panel-specific bits -- geometry, scanline format, the
 * waveform LUTs, and the board's GPIO map -- and hands them to epd_ll_create.
 *
 * Waveform LUT format: one T(...) row == one frame; each of the 16 grayscale
 * columns (0 = black .. 15 = white) gets a 2-bit action:
 *   0 = hold (no drive)   B = drive to black   W = drive to white
 * Refresh replays every row top-to-bottom; the frame count is the array length
 * (no terminator). The CLEAR waveform (uniform drive to white) is the bring-up
 * baseline, run as CLEAR_FULL.
 */

#include "ed047tc1.h"
#include "epd_ll.h"
#include <stdint.h>
#include <stddef.h>

/* Panel geometry + scanline format. ED047TC1 is driven at 2 bits/pixel on the
 * 8-bit source bus, so one scanline is width/4 bytes. */
#define ED047TC1_WIDTH        960
#define ED047TC1_HEIGHT       540
#define ED047TC1_PCLK_HZ      20000000
#define ED047TC1_LINE_BYTES   (ED047TC1_WIDTH / 4)   /* 240 */
#define ED047TC1_LINE_PADDING 16

/* Table-local shorthand: T() packs one frame; B/W name the drive directions
 * (0 = hold). All three are #undef'd after the tables so they don't leak. */
#define T(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df) \
  (uint32_t)((d0<<0)|(d1<<2)|(d2<<4)|(d3<<6)|(d4<<8)|(d5<<10)|(d6<<12)|(d7<<14)| \
             (d8<<16)|(d9<<18)|(da<<20)|(db<<22)|(dc<<24)|(dd<<26)|(de<<28)|(df<<30))
#define B 1   /* drive to black */
#define W 2   /* drive to white */
#define STOP 0u   /* settle frame: every gray holds / column released (no drive) */

/* 16-grayscale, flashing, highest quality (GC16-class). */
static const uint32_t ed047tc1_lut_quality[] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(B, B, B, W, W, W, B, B, W, B, B, B, B, B, B, W),
/*  2 */ T(W, W, W, B, B, W, W, W, B, W, W, W, W, W, W, B),
/*  3 */ T(W, W, W, B, B, 0, W, W, B, B, W, W, W, W, W, B),
/*  4 */ T(W, W, B, 0, W, B, B, B, B, B, B, B, B, B, B, B),
/*  5 */ T(W, W, B, W, W, B, B, B, B, B, B, B, B, B, B, B),
/*  6 */ T(W, 0, 0, W, W, B, B, 0, W, B, B, B, B, 0, B, B),
/*  7 */ T(B, B, W, W, W, B, W, W, W, 0, 0, B, 0, 0, B, W),
/*  8 */ T(B, B, W, B, B, W, W, W, W, B, B, 0, 0, 0, W, W),
/*  9 */ T(B, B, B, B, B, W, B, B, B, W, W, W, W, W, W, W),
/* 10 */ T(B, 0, B, 0, 0, B, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/* 11 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
    /* settle tail (16 frames, no drive) */
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
};

/* 2-level fast direct update: only black/white columns drive, mid-grays held. */
static const uint32_t ed047tc1_lut_fast[] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(W, W, W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B, B, B),
/*  2 */ T(W, W, W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B, B, B),
/*  3 */ T(W, W, W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B, B, B),
/*  4 */ T(W, W, W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B, B, B),
/*  5 */ T(B, B, B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W, W, W),
/*  6 */ T(B, B, B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W, W, W),
/*  7 */ T(B, B, B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W, W, W),
/*  8 */ T(B, B, B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W, W, W),
    /* settle tail (shorter: fast mode trades settle for speed) */
    STOP, STOP, STOP, STOP,
};

/* Drive everything to white. Uniform frames, so (with FULL) refresh reuses one
 * prebuilt scanline for all rows -- a fast way to establish a known white
 * baseline. */
static const uint32_t ed047tc1_lut_clear[] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  2 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  3 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  4 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  5 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  6 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  7 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  8 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  9 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
    /* settle tail (16 frames, no drive) */
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
    STOP, STOP, STOP, STOP, STOP, STOP, STOP, STOP,
};

#undef T
#undef B
#undef W
#undef STOP

static const uint32_t *get_waveform_lut(bsp_epd_mode_t mode, size_t *steps) {
    switch (mode) {
        case BSP_EPD_MODE_FAST:
            *steps = sizeof(ed047tc1_lut_fast) / sizeof(uint32_t);
            return ed047tc1_lut_fast;
        case BSP_EPD_MODE_QUALITY:
            *steps = sizeof(ed047tc1_lut_quality) / sizeof(uint32_t);
            return ed047tc1_lut_quality;
        case BSP_EPD_MODE_CLEAR:
            *steps = sizeof(ed047tc1_lut_clear) / sizeof(uint32_t);
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
    };
    for (int i = 0; i < 8; i++) ec.data_pins[i] = cfg->data_pins[i];
    return epd_ll_create(&ec, out_display);
}
