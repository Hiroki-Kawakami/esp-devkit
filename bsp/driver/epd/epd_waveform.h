/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Per-pixel waveform engine -- the pure (SoC-independent) core of the
 * direct-drive grayscale EPD refresh, factored out of epd_ll.c so it can be
 * unit-tested on the host without ESP-IDF. epd_ll.c owns the i80 bus, the async
 * task, and the locking; this header is just the per-pixel / per-scanline logic.
 *
 * Pixel state (uint16, the only framebuffer):
 *   [15:14] lut_id     waveform slot 0..3
 *   [13:8]  start      engine frame (mod 64) on which the waveform begins
 *   [7:4]   confirmed  on-glass gray (the waveform's `from`)
 *   [3:0]   target     goal gray (the waveform's `to`)
 * The high byte (`b1` = lut_id|start) is the whole life-cycle tag:
 *   0xFF        IDLE     holding; confirmed == target == on-glass value
 *   0xFE        PENDING  target stamped by draw, waveform not yet bound
 *   otherwise   ACTIVE   replaying wf[lut_id] at step = (frame - start) & 63
 * lut_id 3 shares its top-2-bit range with the magics: only b1==0xFE/0xFF are
 * reserved, so slot 3 is a real waveform slot whose caller must skip arming
 * at start_frame 62/63 (epd_ll.c enforces this on the one call site).
 * `step == 63` means "armed": activation always sets start = frame + 1, so a
 * pixel armed mid-scan renders one frame of hold and starts uniformly on the
 * next frame (hence EPD_WF_STEP_MAX = 62, and no per-frame step writeback --
 * the pixel word is immutable while ACTIVE).
 *
 * A waveform LUT is `const uint32_t lut[steps][16]`: one 16-word row per frame
 * indexed by `from`; each word packs a 2-bit action per `to`
 * (0 = hold, 1 = drive to black, 2 = drive to white):
 *   action = (lut[step][from] >> (to * 2)) & 3
 * A frame whose 16 words are all EPD_SETTLE_WORD is a skippable settle frame:
 * it drives nothing (DC-neutral), so when a draw is blocked on an in-flight
 * region the engine may retire pixels whose remaining frames are all settle.
 * An all-zero frame also drives nothing but must run (e.g. the clear
 * waveform's tail) -- the marker is what tells the two apart.
 *
 * An in-flight waveform is otherwise never interrupted: epd_draw_px reports a
 * conflict instead of restamping, and the caller blocks until the pixel
 * retires (see epd_ll.c).
 *
 * Per frame the engine builds two 256-entry tables keyed by b1 (row pointer +
 * retire flag), packs every scanline through them (read-only -- never writes
 * the buffer during the scan, so it cannot race a concurrent draw), and after
 * the scan reclaims retiring pixels to IDLE in a locked pass (epd_retire_row).
 */
#pragma once
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

/* C11 header; `restrict` has no C++ spelling */
#if defined(__cplusplus) && !defined(restrict)
#define restrict __restrict
#endif

#define EPD_WF_SLOTS    4        /* lut_id 0..3; only b1==0xFE/0xFF reserved  */
#define EPD_WF_STEP_MAX 62       /* step 63 = armed-not-started */
#define EPD_B1_IDLE     0xFF
#define EPD_B1_PENDING  0xFE
#define EPD_SETTLE_WORD 0xFFFFFFFFu

typedef struct {
    const uint32_t (*lut)[16];   /* NULL when the slot has no waveform        */
    uint16_t steps;              /* frame count, 1..EPD_WF_STEP_MAX           */
    uint16_t first_skippable;    /* first step of the all-SETTLE tail; == steps
                                    when the tail is not skippable            */
} epd_waveform_t;

/* Bind + validate a LUT: length 1..EPD_WF_STEP_MAX, and no frame may mix
 * EPD_SETTLE_WORD with real words (the marker is per-frame). Locates the
 * skippable settle tail. */
static inline bool epd_waveform_init(epd_waveform_t *wf, const uint32_t (*lut)[16], size_t steps) {
    if (!lut || steps == 0 || steps > EPD_WF_STEP_MAX) return false;
    for (size_t f = 0; f < steps; f++) {
        bool settle = (lut[f][0] == EPD_SETTLE_WORD);
        for (int i = 1; i < 16; i++) {
            if ((lut[f][i] == EPD_SETTLE_WORD) != settle) return false;
        }
    }
    size_t fs = steps;
    while (fs > 0 && lut[fs - 1][0] == EPD_SETTLE_WORD) fs--;
    wf->lut = lut;
    wf->steps = (uint16_t)steps;
    wf->first_skippable = (uint16_t)fs;
    return true;
}

static inline bool epd_px_is_active(uint16_t px) { return (px >> 8) < EPD_B1_PENDING; }

/* b1 for a pixel activated while the engine is at `frame`: starts next frame.
 * For lut_id==3 the caller must ensure (frame+1)&63 is not 62 or 63 -- those
 * two b1 values collide with EPD_B1_PENDING / EPD_B1_IDLE. */
static inline uint8_t epd_b1_armed(int lut_id, unsigned frame) {
    return (uint8_t)((lut_id << 6) | ((frame + 1) & 63));
}

/* Per-pixel draw decision. `px` is the pixel word, `gray` the new target
 * (0..15). Returns the new word:
 *   - target already `gray` (idle showing it, pending or in flight toward it)
 *     -> unchanged; an in-flight pixel is left to finish, never restarted.
 *   - IDLE/PENDING, gray changed -> stamp PENDING with the new target.
 *   - ACTIVE, gray changed -> *conflict; unchanged. The caller must wait for
 *     the pixel to retire and retry (waveforms are never interrupted).
 * *now_nonidle is set on IDLE -> PENDING so the caller can bump its per-row
 * non-idle count. */
static inline uint16_t epd_draw_px(uint16_t px, int gray, bool *conflict, bool *now_nonidle) {
    *conflict = false;
    *now_nonidle = false;
    if (epd_px_is_active(px)) {
        if ((px & 15) != gray) *conflict = true;
        return px;
    }
    if ((px & 15) == gray) return px;               /* diff-skip / same pending */
    if ((px >> 8) == EPD_B1_IDLE) *now_nonidle = true;
    return (uint16_t)((EPD_B1_PENDING << 8) | (px & 0x00F0) | gray);
}

static const uint32_t epd_hold_rowset[16] = {0};    /* every (from,to) holds */

/* Build the per-frame lookup tables keyed by b1: rowset_tab[b1] is the 16-word
 * from-row block to drive this frame (the all-hold block for IDLE / PENDING /
 * armed / settle frames), retire_tab[b1] flags pixels whose waveform ends with
 * this frame (reclaim them after the scan). With `skip_settle`, pixels whose
 * remaining frames are all skippable settle retire early. */
static inline void epd_build_frame_tab(const epd_waveform_t wf[EPD_WF_SLOTS],
                                       uint8_t frame, bool skip_settle,
                                       const uint32_t *rowset_tab[256],
                                       uint8_t retire_tab[256]) {
    for (int b1 = 0; b1 < 256; b1++) {
        const uint32_t *rowset = epd_hold_rowset;
        uint8_t retire = 0;
        /* b1 < EPD_B1_PENDING excludes the 0xFE/0xFF magics; slot 3 uses the
         * rest of that top-2-bit range as a real waveform. */
        if (b1 < EPD_B1_PENDING) {
            int id = b1 >> 6;
            unsigned step = (unsigned)(frame - b1) & 63;
            if (wf[id].lut && step < wf[id].steps) {
                const uint32_t *row = wf[id].lut[step];
                if (row[0] != EPD_SETTLE_WORD) rowset = row;
                retire = (step + 1 >= wf[id].steps) ||
                         (skip_settle && step + 1 >= wf[id].first_skippable);
            } else if (step != 63) {
                retire = 1;   /* unbound slot / stale step: self-heal to IDLE */
            }
        }
        rowset_tab[b1] = rowset;
        retire_tab[b1] = retire;
    }
}

/* Pack one scanline (read-only): `width` pixel words -> width/4 action bytes,
 * 4 px/byte with the leftmost pixel in the high 2-bit pair. Returns true if any
 * pixel drives, so an all-hold row can fall back to a constant line.
 *
 * `width` is a multiple of 4. The hot loop unrolls 8 px/iteration so eight
 * independent load->table->lut chains are in flight at once; `restrict` lets
 * the compiler keep the tables across the dst stores. */
static inline bool epd_blit_line(int width, const uint16_t *restrict state_row,
                                 const uint32_t *const *restrict rowset_tab,
                                 uint8_t *restrict dst) {
#define EPD_ACT(v) ((rowset_tab[(v) >> 8][((v) >> 4) & 15] >> (((v) & 15) * 2)) & 3u)
    uint32_t acc = 0;
    int x = 0;
    for (; x + 8 <= width; x += 8) {
        uint32_t a0 = EPD_ACT(state_row[x + 0]), a1 = EPD_ACT(state_row[x + 1]);
        uint32_t a2 = EPD_ACT(state_row[x + 2]), a3 = EPD_ACT(state_row[x + 3]);
        uint32_t a4 = EPD_ACT(state_row[x + 4]), a5 = EPD_ACT(state_row[x + 5]);
        uint32_t a6 = EPD_ACT(state_row[x + 6]), a7 = EPD_ACT(state_row[x + 7]);
        dst[(x >> 2) + 0] = (uint8_t)((a0 << 6) | (a1 << 4) | (a2 << 2) | a3);
        dst[(x >> 2) + 1] = (uint8_t)((a4 << 6) | (a5 << 4) | (a6 << 2) | a7);
        acc |= a0 | a1 | a2 | a3 | a4 | a5 | a6 | a7;
    }
    for (; x < width; x += 4) {   /* tail when width % 8 == 4 */
        uint32_t a0 = EPD_ACT(state_row[x + 0]), a1 = EPD_ACT(state_row[x + 1]);
        uint32_t a2 = EPD_ACT(state_row[x + 2]), a3 = EPD_ACT(state_row[x + 3]);
        dst[x >> 2] = (uint8_t)((a0 << 6) | (a1 << 4) | (a2 << 2) | a3);
        acc |= a0 | a1 | a2 | a3;
    }
#undef EPD_ACT
    return acc != 0;
}

/* Expand one from-row block into a flat 256-entry action table keyed by the
 * pixel's low byte (confirmed<<4 | target). Lets the scan replace the pointer +
 * LUT-word load + variable shift with one byte load -- and since the caller
 * builds these in internal RAM, the per-pixel reads stop hitting the
 * flash-resident LUTs (which thrash the data cache against the PSRAM state
 * stream). */
static inline void epd_build_act256(const uint32_t *rowset, uint8_t act256[256]) {
    for (int from = 0; from < 16; from++) {
        uint32_t w = rowset[from];
        for (int to = 0; to < 16; to++) {
            act256[(from << 4) | to] = (uint8_t)((w >> (to * 2)) & 3u);
        }
    }
}

/* epd_blit_line variant driven by per-b1 256-byte action tables (built with
 * epd_build_act256; non-driving b1 values point at a shared all-zero table):
 * per pixel, two dependent internal-RAM byte-table loads instead of the
 * word/shift chain. Same packing contract as epd_blit_line. */
static inline bool epd_blit_line_tab(int width, const uint16_t *restrict state_row,
                                     const uint8_t *const *restrict tabs,
                                     uint8_t *restrict dst) {
#define EPD_ACT(v) ((uint32_t)tabs[(v) >> 8][(v) & 0xFF])
    uint32_t acc = 0;
    int x = 0;
    for (; x + 8 <= width; x += 8) {
        uint32_t a0 = EPD_ACT(state_row[x + 0]), a1 = EPD_ACT(state_row[x + 1]);
        uint32_t a2 = EPD_ACT(state_row[x + 2]), a3 = EPD_ACT(state_row[x + 3]);
        uint32_t a4 = EPD_ACT(state_row[x + 4]), a5 = EPD_ACT(state_row[x + 5]);
        uint32_t a6 = EPD_ACT(state_row[x + 6]), a7 = EPD_ACT(state_row[x + 7]);
        dst[(x >> 2) + 0] = (uint8_t)((a0 << 6) | (a1 << 4) | (a2 << 2) | a3);
        dst[(x >> 2) + 1] = (uint8_t)((a4 << 6) | (a5 << 4) | (a6 << 2) | a7);
        acc |= a0 | a1 | a2 | a3 | a4 | a5 | a6 | a7;
    }
    for (; x < width; x += 4) {   /* tail when width % 8 == 4 */
        uint32_t a0 = EPD_ACT(state_row[x + 0]), a1 = EPD_ACT(state_row[x + 1]);
        uint32_t a2 = EPD_ACT(state_row[x + 2]), a3 = EPD_ACT(state_row[x + 3]);
        dst[x >> 2] = (uint8_t)((a0 << 6) | (a1 << 4) | (a2 << 2) | a3);
        acc |= a0 | a1 | a2 | a3;
    }
#undef EPD_ACT
    return acc != 0;
}

/* Reclaim retiring pixels in one state row: every pixel whose b1 is flagged in
 * retire_tab goes IDLE with confirmed = target. Returns the count reclaimed,
 * to decrement the per-row non-idle count. */
static inline int epd_retire_row(int width, uint16_t *restrict state_row,
                                 const uint8_t *restrict retire_tab) {
    int freed = 0;
    for (int x = 0; x < width; x++) {
        uint16_t v = state_row[x];
        if (retire_tab[v >> 8]) {
            uint16_t t = v & 15;
            state_row[x] = (uint16_t)(0xFF00 | (t << 4) | t);
            freed++;
        }
    }
    return freed;
}

/* True when `rowset` drives every (from,to) with the same 2-bit action -- then
 * one prebuilt constant scanline serves every all-driving row (the speed win
 * for the clear waveform's uniform frames). */
static inline bool epd_rowset_uniform(const uint32_t *rowset, uint8_t *line_byte) {
    uint32_t w = rowset[0];
    if (w != 0x00000000u && w != 0x55555555u && w != 0xAAAAAAAAu) return false;
    for (int i = 1; i < 16; i++) {
        if (rowset[i] != w) return false;
    }
    *line_byte = (uint8_t)(w & 0xFF);
    return true;
}
