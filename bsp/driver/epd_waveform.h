/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Waveform scanline core -- the pure (SoC-independent) part of the direct-drive
 * grayscale EPD refresh, factored out of epd_ll.c so it can be unit-tested on the
 * host without ESP-IDF. epd_ll.c owns the i80 bus, the power sequence, and the
 * synchronous refresh loop; this header is just the per-frame / per-scanline
 * pixel logic.
 *
 * Data model (single refresh at a time, no concurrent generations):
 *   gram   -- 4bpp gray, 2 px/byte. The lower-x pixel of a pair is the HIGH
 *             nibble, the higher-x pixel the LOW nibble. This is both the draw
 *             target and the confirmed on-glass value (the waveform is
 *             target-only, so the old gray is never needed once a pixel is drawn).
 *   dirty  -- 1bpp, 8 px/byte, bit (x & 7) (LSB = the pixel whose x % 8 == 0).
 *             Set by draw where the target gray differs from the current nibble;
 *             cleared after a refresh commits. Gates the blit so unchanged pixels
 *             inside a redrawn region hold instead of re-flashing (diff-skip).
 *
 * A refresh replays the mode's LUT frame-by-frame over the whole panel; each
 * frame builds a 16-entry gray->action table (epd_build_act16) and packs every
 * scanline through it (epd_blit_line, gated by dirty / epd_blit_line_full for
 * FULL/CLEAR which drives every pixel by its gray). Output is 2bpp (4 px/byte,
 * leftmost pixel in the high 2-bit pair). Action codes: 0 = hold, 1 = drive to
 * black, 2 = drive to white.
 */
#pragma once
#include <stdint.h>
#include <stdbool.h>

#define EPD_GRAY_LEVELS 16

/* Build the per-frame action table: for each of the 16 gray levels, the 2-bit
 * drive action packed in the frame word (act = (frame >> gray*2) & 3). */
static inline void epd_build_act16(uint32_t frame, uint8_t act16[EPD_GRAY_LEVELS]) {
    for (int g = 0; g < EPD_GRAY_LEVELS; g++) {
        act16[g] = (uint8_t)((frame >> (g * 2)) & 3u);
    }
}

/* Pack one scanline of the diff-skip path: `width` pixels of 4bpp gram, gated by
 * the 1bpp dirty mask, into width/4 output bytes. A clean pixel (dirty bit 0)
 * emits hold (0) regardless of its gray; a dirty pixel emits act16[gray]. Returns
 * true if any pixel drives.
 *
 * `width` is a multiple of 8 (so dirty bytes align). gram_row is width/2 bytes,
 * dirty_row width/8, dst width/4. The 8-px stride lets an all-clean byte skip the
 * gram reads entirely -- the per-region win over a whole-row skip. */
static inline bool epd_blit_line(int width, const uint8_t *restrict gram_row,
                                 const uint8_t *restrict dirty_row,
                                 const uint8_t *restrict act16, uint8_t *restrict dst) {
    int acc = 0;
    for (int x = 0; x < width; x += 8) {
        uint8_t d = dirty_row[x >> 3];
        uint8_t *o = dst + (x >> 2);
        if (d == 0) { o[0] = 0; o[1] = 0; continue; }   /* whole byte clean: hold */
        const uint8_t *g = gram_row + (x >> 1);         /* 8 px = 4 gram bytes */
        uint8_t a[8];
        for (int k = 0; k < 4; k++) {
            uint8_t gb = g[k];
            a[2 * k]     = (d & (1u << (2 * k)))     ? act16[gb >> 4]   : 0;
            a[2 * k + 1] = (d & (1u << (2 * k + 1))) ? act16[gb & 0x0F] : 0;
        }
        o[0] = (uint8_t)((a[0] << 6) | (a[1] << 4) | (a[2] << 2) | a[3]);
        o[1] = (uint8_t)((a[4] << 6) | (a[5] << 4) | (a[6] << 2) | a[7]);
        for (int i = 0; i < 8; i++) acc |= a[i];
    }
    return acc != 0;
}

/* Pack one scanline driving EVERY pixel by its gray nibble (no dirty gate) -- the
 * FULL / CLEAR path. `width` is a multiple of 4; gram_row width/2 bytes. */
static inline bool epd_blit_line_full(int width, const uint8_t *restrict gram_row,
                                      const uint8_t *restrict act16, uint8_t *restrict dst) {
    int acc = 0;
    for (int x = 0; x < width; x += 4) {
        const uint8_t *g = gram_row + (x >> 1);
        int a0 = act16[g[0] >> 4], a1 = act16[g[0] & 0x0F];
        int a2 = act16[g[1] >> 4], a3 = act16[g[1] & 0x0F];
        dst[x >> 2] = (uint8_t)((a0 << 6) | (a1 << 4) | (a2 << 2) | a3);
        acc |= a0 | a1 | a2 | a3;
    }
    return acc != 0;
}
