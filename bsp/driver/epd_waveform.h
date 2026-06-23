/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * transaction-index waveform engine -- the pure (SoC-independent) core of the
 * direct-drive grayscale EPD refresh, factored out of epd_ll.c so it can be
 * unit-tested on the host without ESP-IDF. epd_ll.c owns the i80 bus, the async
 * task, and the locking; this header is just the per-pixel / per-scanline logic.
 *
 * Pixel byte (one per pixel, the only framebuffer): [7:4] = target gray
 * (16 levels, 0 = black .. 15 = white), [3:0] = transaction id.
 *   id 0      -- idle: the pixel holds; its gray nibble is the confirmed on-glass
 *                value (the diff-skip baseline).
 *   id 1..15  -- a refresh generation. Up to 15 progress independently. A slot is
 *                PENDING once draw() has stamped pixels with it, ACTIVE once
 *                refresh() binds a waveform LUT and the engine replays it, then
 *                FREE again once the engine reclaims it.
 *
 * Diff-skip lives in draw, not refresh: epd_draw_pixel compares the new target
 * against the byte's current gray and only stamps an id where they differ -- so
 * the old confirmed gray never needs a second buffer, it is consumed at compare
 * time. refresh() is then O(1) (just bind + activate the open generation).
 *
 * Per frame the engine builds a 256-entry action table keyed by the whole pixel
 * byte, packs every scanline through it (read-only -- never writes the buffer
 * during the scan, so it cannot race a concurrent draw), and on a generation's
 * final frame reclaims its id->0 in a separate locked pass (epd_reclaim_row).
 */
#pragma once
#include <stdint.h>
#include <stdbool.h>

#define EPD_TX_SLOTS 16          /* id 0 is "idle"; ids 1..15 are real generations */

typedef enum {
    EPD_TX_FREE,                 /* slot unused                                    */
    EPD_TX_PENDING,              /* stamped by draw, waveform not yet bound        */
    EPD_TX_ACTIVE,               /* bound + being replayed by the engine           */
} epd_tx_state_t;

typedef struct {
    const uint32_t *lut;         /* waveform, one uint32 per frame (ACTIVE only)   */
    uint16_t        steps;       /* lut length, in frames                          */
    uint16_t        frame;       /* current frame index into lut                   */
    epd_tx_state_t  state;
    bool            ending;      /* the frame being rendered is this gen's last    */
} epd_tx_t;

/* Allocate a free slot (id 1..15), marking it PENDING; returns 0 if none free. */
static inline int epd_tx_alloc(epd_tx_t tx[EPD_TX_SLOTS]) {
    for (int id = 1; id < EPD_TX_SLOTS; id++) {
        if (tx[id].state == EPD_TX_FREE) {
            tx[id] = (epd_tx_t){ .state = EPD_TX_PENDING };
            return id;
        }
    }
    return 0;
}

/* Per-pixel draw decision under the open generation `pending_id` (1..15).
 * `cur` is the existing pixel byte, `gray` the new target (0..15). Returns the
 * new byte:
 *   - target gray unchanged (idle OR in flight) -> unchanged. An idle pixel is
 *     already showing it (diff-skip); an in-flight pixel is already driving to it,
 *     so let that generation finish rather than restart it. This is what keeps a
 *     redraw whose dirty box overlaps an unchanged in-flight region from
 *     re-flashing that region.
 *   - same open generation, gray changed        -> update gray, keep id.
 *   - another generation in flight, gray changed -> interrupt onto (gray, pending_id).
 *   - idle pixel, gray changed                   -> stamp (gray, pending_id).
 * *now_nonidle is set when the pixel goes idle -> stamped (id 0 -> non-zero), so
 * the caller can bump its per-row non-idle count. */
static inline uint8_t epd_draw_pixel(uint8_t cur, int gray, int pending_id, bool *now_nonidle) {
    int old_id   = cur & 0x0F;
    int old_gray = cur >> 4;
    if (old_gray == gray) { *now_nonidle = false; return cur; }
    *now_nonidle = (old_id == 0);
    return (uint8_t)((gray << 4) | pending_id);
}

/* Mark, for the frame about to render, which ACTIVE generations finish on it.
 * Returns true if any slot is ACTIVE (engine keeps running); writes the set of
 * ids ACTIVE right now to *active_mask -- the generations this frame renders and
 * the only ones epd_frame_advance may step -- and the subset finishing this frame
 * to *ended_mask. A generation activated AFTER this call (e.g. during the
 * lock-free scan) is absent from active_mask, so it is left at frame 0 to be
 * rendered next frame instead of being advanced past its own first frame. Call
 * before the table build so next-state reclaim sees the ending flags. */
static inline bool epd_frame_mark(epd_tx_t tx[EPD_TX_SLOTS],
                                  uint16_t *active_mask, uint16_t *ended_mask) {
    uint16_t active = 0, ended = 0;
    for (int id = 1; id < EPD_TX_SLOTS; id++) {
        if (tx[id].state != EPD_TX_ACTIVE) continue;
        active |= (uint16_t)(1u << id);
        tx[id].ending = (tx[id].frame + 1 >= tx[id].steps);
        if (tx[id].ending) ended |= (uint16_t)(1u << id);
    }
    *active_mask = active;
    *ended_mask  = ended;
    return active != 0;
}

/* After a frame has been scanned: among the generations that were rendered this
 * frame (`active_mask` from epd_frame_mark), retire the ones that ended
 * (slot -> FREE) and advance the rest by one frame. Pair with a reclaim pass over
 * the buffer for the ended ids (epd_reclaim_row). */
static inline void epd_frame_advance(epd_tx_t tx[EPD_TX_SLOTS], uint16_t active_mask) {
    for (int id = 1; id < EPD_TX_SLOTS; id++) {
        if (!(active_mask & (1u << id))) continue;
        if (tx[id].ending) tx[id].state = EPD_TX_FREE;
        else tx[id].frame++;
    }
}

/* Build the per-frame action table: for each possible pixel byte [gray|id], the
 * 2-bit drive action (0 = hold, 1 = to black, 2 = to white). Non-ACTIVE ids
 * (idle, pending) decode to hold. */
static inline void epd_build_act_tab(const epd_tx_t tx[EPD_TX_SLOTS], uint8_t act_tab[256]) {
    for (int b = 0; b < 256; b++) {
        int gray = b >> 4, id = b & 0x0F;
        uint32_t word = (tx[id].state == EPD_TX_ACTIVE) ? tx[id].lut[tx[id].frame] : 0u;
        act_tab[b] = (uint8_t)((word >> (gray * 2)) & 3u);
    }
}

/* Pack one scanline (read-only): `width` state bytes -> width/4 action bytes,
 * 4 px/byte with the leftmost pixel in the high 2-bit pair. Returns true if any
 * pixel drives, so an all-hold row can fall back to a constant line. */
static inline bool epd_blit_line(int width, const uint8_t *state_row,
                                 const uint8_t *act_tab, uint8_t *dst) {
    int acc = 0;
    for (int x = 0; x < width; x += 4) {
        int a0 = act_tab[state_row[x + 0]], a1 = act_tab[state_row[x + 1]];
        int a2 = act_tab[state_row[x + 2]], a3 = act_tab[state_row[x + 3]];
        dst[x >> 2] = (uint8_t)((a0 << 6) | (a1 << 4) | (a2 << 2) | a3);
        acc |= a0 | a1 | a2 | a3;
    }
    return acc != 0;
}

/* Pack one scanline driving EVERY pixel by its gray nibble for `frame`, ignoring
 * ids -- the FULL / CLEAR path (no diff, no transactions). */
static inline bool epd_blit_line_full(int width, const uint8_t *state_row,
                                      uint32_t frame, uint8_t *dst) {
    int acc = 0;
    for (int x = 0; x < width; x += 4) {
        int a[4];
        for (int k = 0; k < 4; k++) { int g = state_row[x + k] >> 4; a[k] = (frame >> (g * 2)) & 3; }
        dst[x >> 2] = (uint8_t)((a[0] << 6) | (a[1] << 4) | (a[2] << 2) | a[3]);
        acc |= a[0] | a[1] | a[2] | a[3];
    }
    return acc != 0;
}

/* Reclaim finished generations in one state row: every pixel whose id is set in
 * `ended_mask` reverts to idle (id 0; gray, now the confirmed value, is kept).
 * Returns the count reclaimed, to decrement the per-row non-idle count. */
static inline int epd_reclaim_row(int width, uint8_t *state_row, uint16_t ended_mask) {
    int freed = 0;
    for (int x = 0; x < width; x++) {
        int id = state_row[x] & 0x0F;
        if (id && (ended_mask & (1u << id))) { state_row[x] &= 0xF0; freed++; }
    }
    return freed;
}
