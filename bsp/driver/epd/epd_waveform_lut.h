/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Waveform-LUT authoring macros for epd_ll panel descriptors. A LUT is
 * `const uint32_t name[][16]`: one row of 16 words per frame, indexed by the
 * on-glass gray (`from`); each word packs a 2-bit action per target gray
 * (`to`): action = (lut[step][from] >> (to * 2)) & 3.
 *
 * Defines single-letter macros (I, F, T, B, W, STOP, SETTLE) -- include ONLY
 * in the file that defines the waveform tables, after every other include.
 */
#pragma once
#include "epd_waveform.h"

#define B 1   /* drive to black */
#define W 2   /* drive to white */

/* One word: the 2-bit action for each target gray (to = 0..15). */
#define I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df) \
  (((uint32_t)(d0)<< 0)|((uint32_t)(d1)<< 2)|((uint32_t)(d2)<< 4)|((uint32_t)(d3)<< 6)| \
   ((uint32_t)(d4)<< 8)|((uint32_t)(d5)<<10)|((uint32_t)(d6)<<12)|((uint32_t)(d7)<<14)| \
   ((uint32_t)(d8)<<16)|((uint32_t)(d9)<<18)|((uint32_t)(da)<<20)|((uint32_t)(db)<<22)| \
   ((uint32_t)(dc)<<24)|((uint32_t)(dd)<<26)|((uint32_t)(de)<<28)|((uint32_t)(df)<<30))

/* One frame whose action depends only on the CURRENT (from) gray: argument N
 * drives every pixel whose on-glass gray is N, whatever its target. */
#define F(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df) { \
    I(d0,d0,d0,d0,d0,d0,d0,d0,d0,d0,d0,d0,d0,d0,d0,d0), \
    I(d1,d1,d1,d1,d1,d1,d1,d1,d1,d1,d1,d1,d1,d1,d1,d1), \
    I(d2,d2,d2,d2,d2,d2,d2,d2,d2,d2,d2,d2,d2,d2,d2,d2), \
    I(d3,d3,d3,d3,d3,d3,d3,d3,d3,d3,d3,d3,d3,d3,d3,d3), \
    I(d4,d4,d4,d4,d4,d4,d4,d4,d4,d4,d4,d4,d4,d4,d4,d4), \
    I(d5,d5,d5,d5,d5,d5,d5,d5,d5,d5,d5,d5,d5,d5,d5,d5), \
    I(d6,d6,d6,d6,d6,d6,d6,d6,d6,d6,d6,d6,d6,d6,d6,d6), \
    I(d7,d7,d7,d7,d7,d7,d7,d7,d7,d7,d7,d7,d7,d7,d7,d7), \
    I(d8,d8,d8,d8,d8,d8,d8,d8,d8,d8,d8,d8,d8,d8,d8,d8), \
    I(d9,d9,d9,d9,d9,d9,d9,d9,d9,d9,d9,d9,d9,d9,d9,d9), \
    I(da,da,da,da,da,da,da,da,da,da,da,da,da,da,da,da), \
    I(db,db,db,db,db,db,db,db,db,db,db,db,db,db,db,db), \
    I(dc,dc,dc,dc,dc,dc,dc,dc,dc,dc,dc,dc,dc,dc,dc,dc), \
    I(dd,dd,dd,dd,dd,dd,dd,dd,dd,dd,dd,dd,dd,dd,dd,dd), \
    I(de,de,de,de,de,de,de,de,de,de,de,de,de,de,de,de), \
    I(df,df,df,df,df,df,df,df,df,df,df,df,df,df,df,df), \
}

/* One frame whose action depends only on the TARGET (to) gray -- the classic
 * to-indexed row, identical for every from. */
#define T(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df) { \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
    I(d0,d1,d2,d3,d4,d5,d6,d7,d8,d9,da,db,dc,dd,de,df), \
}

/* Settle frames (no drive). STOP always runs to completion; SETTLE may be
 * skipped by the engine to unblock a waiting draw (DC-neutral). */
#define STOP T(0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0)
#define SETTLE { \
    EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, \
    EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, \
    EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, \
    EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, EPD_SETTLE_WORD, \
}
