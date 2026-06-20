/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ED047TC1 panel-intrinsic parameters: geometry, scanline format, and the
 * time-axis waveform LUTs. Included only by ed047tc1.c.
 *
 * Waveform LUT format: one row (T(...)) == one frame; each of the 16 grayscale
 * columns (0 = black .. 15 = white) gets a 2-bit action:
 *   0 = hold (no drive)   B = drive to black   W = drive to white
 * Refresh replays every row top-to-bottom; the frame count is just the array
 * length (no terminator).
 */

#pragma once
#include <stdint.h>
#include <stddef.h>

/* Panel geometry + scanline format. ED047TC1 is driven at 2 bits/pixel on the
 * 8-bit source bus, so one scanline is width/4 bytes. */
#define ED047TC1_WIDTH        960
#define ED047TC1_HEIGHT       540
#define ED047TC1_PCLK_HZ      16000000
#define ED047TC1_LINE_BYTES   (ED047TC1_WIDTH / 4)   /* 240 */
#define ED047TC1_LINE_PADDING 8

/* Table-local shorthand: T() packs one frame; B/W name the drive directions
 * (0 = hold). All three are #undef'd after the tables so they don't leak. */
#define T(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df) \
  (uint32_t)((d0<<0)|(d1<<2)|(d2<<4)|(d3<<6)|(d4<<8)|(d5<<10)|(d6<<12)|(d7<<14)| \
             (d8<<16)|(d9<<18)|(da<<20)|(db<<22)|(dc<<24)|(dd<<26)|(de<<28)|(df<<30))
#define B 1   /* drive to black */
#define W 2   /* drive to white */

/* 16-grayscale, flashing, highest quality (GC16-class). */
static const uint32_t ed047tc1_lut_quality[] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(B, B, B, B, B, B, B, W, B, W, W, B, B, B, B, B),
/*  2 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  3 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  4 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  5 */ T(B, B, B, B, B, B, B, B, B, B, B, B, B, B, B, B),
/*  6 */ T(B, B, B, B, B, B, B, B, W, B, B, B, B, B, B, B),
/*  7 */ T(B, B, W, W, B, B, B, W, B, W, B, B, B, B, B, 0),
/*  8 */ T(B, B, B, B, B, W, B, B, W, W, B, W, B, W, W, W),
/*  9 */ T(B, B, 0, W, W, B, W, W, W, W, W, W, B, B, W, W),
/* 10 */ T(0, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/* 11 */ T(0, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/* 12 */ T(B, B, B, B, 0, W, W, W, W, W, W, W, W, W, W, W),
/* 13 */ T(B, B, B, B, B, B, B, B, B, B, 0, 0, W, W, W, W),
/* 14 */ T(0, B, B, B, B, B, B, B, B, B, B, B, B, B, B, 0),
/* 15 */ T(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W, W, W, 0),
};

/* 2-level fast direct update: only black/white columns drive, mid-grays held. */
static const uint32_t ed047tc1_lut_fast[] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B),
/*  2 */ T(W, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B),
/*  3 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  4 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  5 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  6 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  7 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
/*  8 */ T(B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W),
};

/* Drive everything to white. Uniform frames, so refresh reuses one prebuilt
 * scanline for all rows -- a fast way to establish a known white baseline. */
static const uint32_t ed047tc1_lut_clear[] = {
/*         0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15   (gray column) */
/*  1 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  2 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  3 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
/*  4 */ T(W, W, W, W, W, W, W, W, W, W, W, W, W, W, W, W),
};

#undef T
#undef B
#undef W

#define ED047TC1_LUT_QUALITY_STEPS (sizeof(ed047tc1_lut_quality) / sizeof(uint32_t))
#define ED047TC1_LUT_FAST_STEPS    (sizeof(ed047tc1_lut_fast)    / sizeof(uint32_t))
#define ED047TC1_LUT_CLEAR_STEPS   (sizeof(ed047tc1_lut_clear)   / sizeof(uint32_t))
