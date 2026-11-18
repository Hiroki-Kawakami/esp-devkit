/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S3 PIE kernels of simd/mod.rs; its scalar loops write the same pixels.
 */

/* q3 = q1 over q0 by q7 = a5 and q6 = 32 - a5 in each field, which is what
 * mix565 computes: (front * a5 + back * (32 - a5) + 16) / 32. Fields are taken
 * apart and put back with VMUL.U16, which shifts each product right by SAR:
 * there is no 16-bit lane shift, and VADDS saturates, so every sum stays small.
 * Reads the 12 constants of BLEND_CONSTS through a8, back where it was. */
.macro MIX565
    /* red */
    ssai 11
    ee.vld.128.ip q2, a8, 16                /* 1 */
    ee.vmul.u16 q4, q0, q2
    ee.vmul.u16 q5, q1, q2
    ssai 0
    ee.vmul.u16 q4, q4, q6
    ee.vmul.u16 q5, q5, q7
    ee.vadds.s16 q4, q4, q5
    ee.vld.128.ip q2, a8, 16                /* 16 */
    ee.vadds.s16 q4, q4, q2
    ssai 5
    ee.vld.128.ip q2, a8, 16                /* 1 */
    ee.vmul.u16 q4, q4, q2
    ssai 0
    ee.vld.128.ip q2, a8, 16                /* 2048 */
    ee.vmul.u16 q3, q4, q2
    /* green */
    ssai 5
    ee.vld.128.ip q2, a8, 16                /* 1 */
    ee.vmul.u16 q4, q0, q2
    ee.vmul.u16 q5, q1, q2
    ee.vld.128.ip q2, a8, 16                /* 0x3f */
    ee.andq q4, q4, q2
    ee.andq q5, q5, q2
    ssai 0
    ee.vmul.u16 q4, q4, q6
    ee.vmul.u16 q5, q5, q7
    ee.vadds.s16 q4, q4, q5
    ee.vld.128.ip q2, a8, 16                /* 16 */
    ee.vadds.s16 q4, q4, q2
    ssai 5
    ee.vld.128.ip q2, a8, 16                /* 1 */
    ee.vmul.u16 q4, q4, q2
    ssai 0
    ee.vld.128.ip q2, a8, 16                /* 32 */
    ee.vmul.u16 q4, q4, q2
    ee.orq q3, q3, q4
    /* blue */
    ee.vld.128.ip q2, a8, 16                /* 0x1f */
    ee.andq q4, q0, q2
    ee.andq q5, q1, q2
    ee.vmul.u16 q4, q4, q6
    ee.vmul.u16 q5, q5, q7
    ee.vadds.s16 q4, q4, q5
    ee.vld.128.ip q2, a8, 16                /* 16 */
    ee.vadds.s16 q4, q4, q2
    ssai 5
    ee.vld.128.ip q2, a8, -176              /* 1 */
    ee.vmul.u16 q4, q4, q2
    ee.orq q3, q3, q4
.endm

/* 16 bytes at \at, any alignment, into \q, and \at moves 16 on; uses q2. */
.macro LOADU q, at
    ee.ld.128.usar.ip \q, \at, 16
    ee.vld.128.ip q2, \at, 0
    ee.src.q \q, \q, q2
.endm

/* q7 = a5 and q6 = 32 - a5 through the stack. */
.macro WEIGHTS a5
    movi a9, 32
    sub a9, a9, \a5
    s16i \a5, a1, 0
    s16i a9, a1, 2
    mov a9, a1
    ee.vldbc.16.ip q7, a9, 2
    ee.vldbc.16.ip q6, a9, 0
.endm

    .text

/* void gfx_s3_blend565(uint16_t *out, const uint16_t *back,
 *                      const uint16_t *front, int n8, int a5,
 *                      const uint16_t *consts)
 * n8 groups of 8 RGB565 pixels; out 16-byte aligned and may be back. back and
 * front are read 16 bytes past each group. */
    .balign 4
    .global gfx_s3_blend565
    .type gfx_s3_blend565, @function
gfx_s3_blend565:
    entry a1, 32
    WEIGHTS a6
    mov a8, a7
    beqz a5, 2f
1:
    LOADU q0, a3
    LOADU q1, a4
    MIX565
    ee.vst.128.ip q3, a2, 16
    addi a5, a5, -1
    bnez a5, 1b
2:
    retw
    .size gfx_s3_blend565, .-gfx_s3_blend565

/* void gfx_s3_tint565(uint16_t *out, int n8, int color, int a5,
 *                     const uint16_t *consts)
 * color over n8 groups of 8 RGB565 pixels in place; out 16-byte aligned. */
    .balign 4
    .global gfx_s3_tint565
    .type gfx_s3_tint565, @function
gfx_s3_tint565:
    entry a1, 32
    WEIGHTS a5
    s16i a4, a1, 4
    addi a9, a1, 4
    ee.vldbc.16.ip q1, a9, 0
    mov a8, a6
    mov a10, a2
    beqz a3, 2f
1:
    ee.vld.128.ip q0, a10, 16
    MIX565
    ee.vst.128.ip q3, a2, 16
    addi a3, a3, -1
    bnez a3, 1b
2:
    retw
    .size gfx_s3_tint565, .-gfx_s3_tint565

/* One half of a tint888 block, 8 bytes widened in \q: (v * w + c * a) >> 8
 * in each, as the high and low bytes of both products, which keeps the sums
 * small: q7 = w, q6 = 0xff, q5 = 1, and a8 reads the low then the high bytes
 * of c * a. Ends with SAR at 8. */
.macro TINT888_HALF q
    ssai 8
    ee.vmul.u16 q3, \q, q7
    ssai 0
    ee.vmul.u16 q4, \q, q7
    ee.andq q4, q4, q6
    ee.vld.128.ip q2, a8, 16
    ee.vadds.s16 q4, q4, q2
    ssai 8
    ee.vmul.u16 q4, q4, q5
    ee.vadds.s16 q3, q3, q4
    ee.vld.128.ip q2, a8, 16
    ee.vadds.s16 \q, q3, q2
.endm

/* void gfx_s3_tint888(uint8_t *out, int n16, const uint16_t *table, int w)
 * A color over n16 groups of 16 B, G, R pixels in place by a of 256, out
 * 16-byte aligned: (pixel * w + color * a) >> 8 in each byte with w = 256 - a,
 * which is what blend888 computes. table holds 0xff, 1, then for each 8 bytes
 * of the 48 of a group, (color * a) & 0xff and (color * a) >> 8. */
    .balign 4
    .global gfx_s3_tint888
    .type gfx_s3_tint888, @function
gfx_s3_tint888:
    entry a1, 32
    s16i a5, a1, 0
    mov a9, a1
    ee.vldbc.16.ip q7, a9, 0
    ee.vld.128.ip q6, a4, 16
    ee.vld.128.ip q5, a4, 16
    mov a10, a2
    beqz a3, 2f
1:
    mov a8, a4
    movi a11, 3
3:
    ee.vld.128.ip q0, a10, 16
    ee.zero.q q1
    ee.vzip.8 q0, q1
    TINT888_HALF q0
    TINT888_HALF q1
    ee.vunzip.8 q0, q1
    ee.vst.128.ip q0, a2, 16
    addi a11, a11, -1
    bnez a11, 3b
    addi a3, a3, -1
    bnez a3, 1b
2:
    retw
    .size gfx_s3_tint888, .-gfx_s3_tint888

/* void gfx_s3_swap565(uint8_t *pixels, int n8, const uint32_t *mask)
 * Reverses the bytes of n8 groups of 8 16-bit pixels in place; pixels 16-byte
 * aligned, mask four 0x00ff00ff. VSR.32 is arithmetic, so the mask also
 * clears what it brings in. */
    .balign 4
    .global gfx_s3_swap565
    .type gfx_s3_swap565, @function
gfx_s3_swap565:
    entry a1, 32
    ee.vld.128.ip q7, a4, 0
    ssai 8
    mov a10, a2
    beqz a3, 2f
1:
    ee.vld.128.ip q0, a10, 16
    ee.andq q1, q0, q7
    ee.vsl.32 q1, q1
    ee.vsr.32 q0, q0
    ee.andq q0, q0, q7
    ee.orq q0, q0, q1
    ee.vst.128.ip q0, a2, 16
    addi a3, a3, -1
    bnez a3, 1b
2:
    retw
    .size gfx_s3_swap565, .-gfx_s3_swap565
