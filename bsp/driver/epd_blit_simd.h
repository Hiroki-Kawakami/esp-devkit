/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * PIE (ESP32-S3 128-bit vector unit) accelerated scanline blit for the FULL/CLEAR
 * path. The scalar reference lives in epd_waveform.h (host unit-tested); this is
 * the device-only fast path, verified against that reference at runtime on real
 * hardware (the simulator has no PIE) -- see the selftest in epd_ll.c.
 *
 * PIE has no gather/shuffle, so the gray->action map cannot be a table lookup.
 * Instead the action is built with byte compares: per frame we list which gray
 * nibbles drive black vs white (epd_simd_frame_t), then for each 16-px vector OR
 * together an equality mask per driving gray. 16 px processed per VCMP.
 */
#pragma once
#include <stdint.h>

/* Per-frame driving-gray lists. bvals/wvals hold the masked byte (gray<<4) of
 * each gray level whose action this frame is black / white. Rebuilt once per
 * frame; reused for every row. */
typedef struct {
    uint8_t bvals[16];
    uint8_t wvals[16];
    int     nb, nw;
} epd_simd_frame_t;

static inline void epd_simd_frame_init(epd_simd_frame_t *f, uint32_t frame) {
    f->nb = f->nw = 0;
    for (int g = 0; g < 16; g++) {
        int a = (frame >> (g * 2)) & 3;
        if      (a == 1) f->bvals[f->nb++] = (uint8_t)(g << 4);   /* drive black */
        else if (a == 2) f->wvals[f->nw++] = (uint8_t)(g << 4);   /* drive white */
    }
}

#if defined(__XTENSA__)

/* Drive every pixel by its gray nibble (ids ignored) -- the FULL/CLEAR blit.
 * `width` must be a multiple of 16; `state_row` and `dst` 16-byte aligned.
 *
 * q-reg map: q0=state, q1=gray<<4, q2=black mask, q3=white mask, q4=cmp scratch,
 * q5=0xF0, q6=0x01, q7=0x02. The 16 action bytes (0/1/2) are stored to a stack
 * vector, then packed 4-px/byte (leftmost px in the high 2-bit pair).
 *
 * Pack: for 4 action bytes a0..a3 (each 0..2) read as a little-endian word
 * L = a0|a1<<8|a2<<16|a3<<24, the packed byte is (L * 0x40100401) >> 24. The
 * magic constant lays each 2-bit field, gap-free and collision-free, into the top
 * byte: a_i (from bit 8i) lands at bit 30-2i via the 2^(30-10i) term, so >>24
 * yields a0<<6|a1<<4|a2<<2|a3. */
#define EPD_SIMD_PACK4(word) (uint8_t)(((uint32_t)(word) * 0x40100401u) >> 24)
static inline void epd_blit_line_full_simd(int width, const uint8_t *state_row,
                                           const epd_simd_frame_t *f, uint8_t *dst) {
    static const uint8_t k_f0 = 0xF0, k_01 = 0x01, k_02 = 0x02;
    for (int x = 0; x < width; x += 16) {
        uint8_t acts[16] __attribute__((aligned(16)));
        const uint8_t *sp = state_row + x;
        const uint8_t *bp = f->bvals, *wp = f->wvals;
        uint8_t *ap = acts;
        int nb = f->nb, nw = f->nw;
        __asm__ volatile(
            "  EE.VLDBC.8     q5, %[f0]              \n"
            "  EE.VLDBC.8     q6, %[c1]              \n"
            "  EE.VLDBC.8     q7, %[c2]              \n"
            "  EE.VLD.128.IP  q0, %[sp], 16          \n"
            "  EE.ANDQ        q1, q0, q5             \n"  /* gray<<4 */
            "  EE.ZERO.Q      q2                     \n"  /* black mask */
            "  EE.ZERO.Q      q3                     \n"  /* white mask */
            "  beqz  %[nb], .Lbdone%=                \n"
            ".Lbloop%=:                              \n"
            "  EE.VLDBC.8.IP  q4, %[bp], 1           \n"
            "  EE.VCMP.EQ.S8  q4, q1, q4             \n"
            "  EE.ORQ         q2, q2, q4             \n"
            "  addi  %[nb], %[nb], -1                \n"
            "  bnez  %[nb], .Lbloop%=                \n"
            ".Lbdone%=:                              \n"
            "  beqz  %[nw], .Lwdone%=                \n"
            ".Lwloop%=:                              \n"
            "  EE.VLDBC.8.IP  q4, %[wp], 1           \n"
            "  EE.VCMP.EQ.S8  q4, q1, q4             \n"
            "  EE.ORQ         q3, q3, q4             \n"
            "  addi  %[nw], %[nw], -1                \n"
            "  bnez  %[nw], .Lwloop%=                \n"
            ".Lwdone%=:                              \n"
            "  EE.ANDQ        q2, q2, q6             \n"  /* 0x01 where black */
            "  EE.ANDQ        q3, q3, q7             \n"  /* 0x02 where white */
            "  EE.ORQ         q2, q2, q3             \n"  /* action 0/1/2 */
            "  EE.VST.128.IP  q2, %[ap], 16          \n"
            : [sp] "+r"(sp), [bp] "+r"(bp), [wp] "+r"(wp), [ap] "+r"(ap),
              [nb] "+r"(nb), [nw] "+r"(nw)
            : [f0] "r"(&k_f0), [c1] "r"(&k_01), [c2] "r"(&k_02)
            : "memory");
        const uint32_t *aw = (const uint32_t *)acts;   /* acts is 16-byte aligned */
        uint8_t *d = dst + (x >> 2);
        d[0] = EPD_SIMD_PACK4(aw[0]);
        d[1] = EPD_SIMD_PACK4(aw[1]);
        d[2] = EPD_SIMD_PACK4(aw[2]);
        d[3] = EPD_SIMD_PACK4(aw[3]);
    }
}

#endif /* __XTENSA__ */
