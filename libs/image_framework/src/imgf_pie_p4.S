/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-P4 PIE kernels. Every kernel has a C counterpart in the same file as
 * its caller and writes exactly the same bytes; imgf_bench checks that on the
 * board.
 */

    .section .rodata
    .balign 16
imgf_ycc_consts:
    .short 256      /* Y  */
    .short 359      /* Cr -> R   (1.402   * 256) */
    .short -88      /* Cb -> G  (-0.34414 * 256) */
    .short -183     /* Cr -> G  (-0.71414 * 256) */
    .short 454      /* Cb -> B   (1.772   * 256) */
    .short 0, 0, 0

    .balign 16
    .global imgf_idct_tab8
imgf_idct_tab8:
    /* full block: pass 1, row u = averaged basis for frequency u */
    .short   5793,   5793,   5793,   5793,   5793,   5793,   5793,   5793
    .short   8035,   6811,   4551,   1598,  -1598,  -4551,  -6811,  -8035
    .short   7568,   3135,  -3135,  -7568,  -7568,  -3135,   3135,   7568
    .short   6811,  -1598,  -8035,  -4551,   4551,   8035,   1598,  -6811
    .short   5793,  -5793,  -5793,   5793,   5793,  -5793,  -5793,   5793
    .short   4551,  -8035,   1598,   6811,  -6811,  -1598,   8035,  -4551
    .short   3135,  -7568,   7568,  -3135,  -3135,   7568,  -7568,   3135
    .short   1598,  -4551,   6811,  -8035,   8035,  -6811,   4551,  -1598
    /* pass 2, row y = column y of the same matrix */
    .short   5793,   8035,   7568,   6811,   5793,   4551,   3135,   1598
    .short   5793,   6811,   3135,  -1598,  -5793,  -8035,  -7568,  -4551
    .short   5793,   4551,  -3135,  -8035,  -5793,   1598,   7568,   6811
    .short   5793,   1598,  -7568,  -4551,   5793,   6811,  -3135,  -8035
    .short   5793,  -1598,  -7568,   4551,   5793,  -6811,  -3135,   8035
    .short   5793,  -4551,  -3135,   8035,  -5793,  -1598,   7568,  -6811
    .short   5793,  -6811,   3135,   1598,  -5793,   8035,  -7568,   4551
    .short   5793,  -8035,   7568,  -6811,   5793,  -4551,   3135,  -1598
    /* +256 pass 1 rounding, +272 and +288 pass 2 rounding + level shift */
    .short   2048,   2048,   2048,   2048,   2048,   2048,   2048,   2048
    .short  16384,  16384,  16384,  16384,  16384,  16384,  16384,  16384
    .short    514, 0, 0, 0, 0, 0, 0, 0

    .balign 16
    .global imgf_idct_tab4
imgf_idct_tab4:
    /* 4x4 from the whole block: pass 1, row u = averaged basis for frequency u */
    .short   5793,   5793,   5793,   5793,      0,      0,      0,      0
    .short   7423,   3075,  -3075,  -7423,      0,      0,      0,      0
    .short   5352,  -5352,  -5352,   5352,      0,      0,      0,      0
    .short   2607,  -6293,   6293,  -2607,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short  -1742,   4205,  -4205,   1742,      0,      0,      0,      0
    .short  -2217,   2217,   2217,  -2217,      0,      0,      0,      0
    .short  -1477,   -612,    612,   1477,      0,      0,      0,      0
    /* pass 2, row y = column y of the same matrix */
    .short   5793,   7423,   5352,   2607,      0,  -1742,  -2217,  -1477
    .short   5793,   3075,  -5352,  -6293,      0,   4205,   2217,   -612
    .short   5793,  -3075,  -5352,   6293,      0,  -4205,   2217,    612
    .short   5793,  -7423,   5352,  -2607,      0,   1742,  -2217,   1477
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    /* +256 pass 1 rounding, +272 and +288 pass 2 rounding + level shift */
    .short   2048,   2048,   2048,   2048,   2048,   2048,   2048,   2048
    .short  16384,  16384,  16384,  16384,  16384,  16384,  16384,  16384
    .short    514, 0, 0, 0, 0, 0, 0, 0

    .balign 16
    .global imgf_idct_tab2
imgf_idct_tab2:
    /* 2x2 from the whole block: pass 1, row u = averaged basis for frequency u */
    .short   5793,   5793,      0,      0,      0,      0,      0,      0
    .short   5249,  -5249,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short  -1843,   1843,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short   1232,  -1232,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short  -1044,   1044,      0,      0,      0,      0,      0,      0
    /* pass 2, row y = column y of the same matrix */
    .short   5793,   5249,      0,  -1843,      0,   1232,      0,  -1044
    .short   5793,  -5249,      0,   1843,      0,  -1232,      0,   1044
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    .short      0,      0,      0,      0,      0,      0,      0,      0
    /* +256 pass 1 rounding, +272 and +288 pass 2 rounding + level shift */
    .short   2048,   2048,   2048,   2048,   2048,   2048,   2048,   2048
    .short  16384,  16384,  16384,  16384,  16384,  16384,  16384,  16384
    .short    514, 0, 0, 0, 0, 0, 0, 0
    .text

/* qlo/qhi hold one 16-pixel channel as two halves of 8 x s16; leaves the
 * 16 packed bytes in qlo. */
.macro PACK8 qlo, qhi
    esp.vsat.s16 \qlo, \qlo, s0, s1
    esp.vsat.s16 \qhi, \qhi, s0, s1
    esp.vunzip.8 \qlo, \qhi
.endm

/* Widen 8 chroma bytes at \ptr to s16, duplicate each one so it covers two
 * luma samples, and bias by -128. Advances \ptr by 8. */
.macro CHROMA_H1 qlo, qhi, ptr
    esp.vldext.u8.ip \qlo, \qhi, \ptr, 0
    addi \ptr, \ptr, 8
    esp.orq \qhi, \qlo, \qlo
    esp.vzip.16 \qlo, \qhi
    esp.vsadds.s16 \qlo, \qlo, t3
    esp.vsadds.s16 \qhi, \qhi, t3
.endm

/* Same for chroma that is already at luma resolution. Advances \ptr by 16. */
.macro CHROMA_H0 qlo, qhi, ptr
    esp.vldext.u8.ip \qlo, \qhi, \ptr, 16
    esp.vsadds.s16 \qlo, \qlo, t3
    esp.vsadds.s16 \qhi, \qhi, t3
.endm

/* void imgf_k_ycc_rgb_h1(uint8_t *dst, const uint8_t *y,
 *                        const uint8_t *cb, const uint8_t *cr, int n16)
 * void imgf_k_ycc_rgb_h0(...)
 *
 * n16 groups of 16 pixels. The chroma loads read 16 bytes to use 8, so the
 * planes need 16 bytes of slack after the last row.
 */
.macro YCC_BODY chroma
    addi sp, sp, -16
    sw s0, 0(sp)
    sw s1, 4(sp)
    la t4, imgf_ycc_consts
    esp.vld.128.ip q7, t4, 0
    li s0, 0
    li s1, 255
    li t3, -128
    li t5, 8                                /* Q8 -> byte */
    blez a4, 2f
1:
    esp.vldext.u8.ip q0, q1, a1, 16         /* q0/q1 = Y */
    \chroma q2, q3, a3                      /* q2/q3 = Cr - 128 */

    esp.zero.qacc                           /* R = (Y*256 + Cr*359) >> 8 */
    esp.vsmulas.s16.qacc q0, q7, 0
    esp.vsmulas.s16.qacc q2, q7, 1
    esp.srcmb.s16.qacc q4, t5, 0
    esp.zero.qacc
    esp.vsmulas.s16.qacc q1, q7, 0
    esp.vsmulas.s16.qacc q3, q7, 1
    esp.srcmb.s16.qacc q5, t5, 0
    PACK8 q4, q5                            /* q4 = 16 R bytes */

    \chroma q5, q6, a2                      /* q5/q6 = Cb - 128 */

    esp.zero.qacc                           /* G, over Cr in q2/q3 */
    esp.vsmulas.s16.qacc q0, q7, 0
    esp.vsmulas.s16.qacc q5, q7, 2
    esp.vsmulas.s16.qacc q2, q7, 3
    esp.srcmb.s16.qacc q2, t5, 0
    esp.zero.qacc
    esp.vsmulas.s16.qacc q1, q7, 0
    esp.vsmulas.s16.qacc q6, q7, 2
    esp.vsmulas.s16.qacc q3, q7, 3
    esp.srcmb.s16.qacc q3, t5, 0
    PACK8 q2, q3                            /* q2 = 16 G bytes */

    esp.zero.qacc                           /* B, over Y in q0/q1 */
    esp.vsmulas.s16.qacc q0, q7, 0
    esp.vsmulas.s16.qacc q5, q7, 4
    esp.srcmb.s16.qacc q3, t5, 0
    esp.zero.qacc
    esp.vsmulas.s16.qacc q1, q7, 0
    esp.vsmulas.s16.qacc q6, q7, 4
    esp.srcmb.s16.qacc q0, t5, 0
    PACK8 q3, q0                            /* q3 = 16 B bytes */

    esp.vzipt.8 q4, q2, q3
    esp.vst.128.ip q4, a0, 16
    esp.vst.128.ip q2, a0, 16
    esp.vst.128.ip q3, a0, 16

    addi a4, a4, -1
    bnez a4, 1b
2:
    lw s0, 0(sp)
    lw s1, 4(sp)
    addi sp, sp, 16
    ret
.endm

    .balign 4
    .global imgf_k_ycc_rgb_h1
    .type imgf_k_ycc_rgb_h1, @function
imgf_k_ycc_rgb_h1:
    YCC_BODY CHROMA_H1
    .size imgf_k_ycc_rgb_h1, .-imgf_k_ycc_rgb_h1

    .balign 4
    .global imgf_k_ycc_rgb_h0
    .type imgf_k_ycc_rgb_h0, @function
imgf_k_ycc_rgb_h0:
    YCC_BODY CHROMA_H0
    .size imgf_k_ycc_rgb_h0, .-imgf_k_ycc_rgb_h0

/* void imgf_k_idct(uint8_t *dst, int stride, const int16_t *dqc, int rowmask,
 *                  const int16_t *tab)
 *
 * Always writes 8 lines of 8 bytes; the sub-sampled tables leave the answer in
 * the top-left n x n and fill the rest with whatever the zero rows produce, so
 * those callers pass an 8x8 scratch. The row pass writes its lines to a stack
 * block, so the task's stack has to be SIMD-capable like everything else a
 * kernel touches.
 */
.macro MAC8 acc_src, acc_other, ptr, scalars
    esp.vsmulas.s16.qacc.ld.incp \acc_other, \ptr, \acc_src, \scalars, 0
    esp.vsmulas.s16.qacc.ld.incp \acc_src, \ptr, \acc_other, \scalars, 1
    esp.vsmulas.s16.qacc.ld.incp \acc_other, \ptr, \acc_src, \scalars, 2
    esp.vsmulas.s16.qacc.ld.incp \acc_src, \ptr, \acc_other, \scalars, 3
    esp.vsmulas.s16.qacc.ld.incp \acc_other, \ptr, \acc_src, \scalars, 4
    esp.vsmulas.s16.qacc.ld.incp \acc_src, \ptr, \acc_other, \scalars, 5
    esp.vsmulas.s16.qacc.ld.incp \acc_other, \ptr, \acc_src, \scalars, 6
    esp.vsmulas.s16.qacc \acc_other, \scalars, 7
.endm

/* One output line of the column pass into \out. */
.macro COL_LINE out
    esp.vld.128.ip q0, s9, 16               /* transposed matrix row */
    esp.zero.qacc
    esp.vsmulas.s16.qacc q4, q5, 0          /* rounding + level shift */
    mv t4, sp
    esp.vld.128.ip q1, t4, 16
    MAC8 q1, q2, t4, q0
    esp.srcmb.s16.qacc \out, s8, 0
.endm

    .balign 4
    .global imgf_k_idct
    .type imgf_k_idct, @function
imgf_k_idct:
    addi sp, sp, -160
    sw s0, 128(sp)
    sw s1, 132(sp)
    sw s8, 136(sp)
    sw s9, 140(sp)
    sw s10, 144(sp)
    sw s11, 148(sp)

    mv t3, a4
    addi t4, t3, 256
    esp.vld.128.ip q4, t4, 0                /* q4 = 2048 */
    li s8, 12
    mv s9, sp
    li s10, 8
    mv s11, a3
    esp.zero.q q5
1:
    andi t6, s11, 1
    beqz t6, 2f
    esp.vld.128.ip q1, a2, 16               /* dequantized coefficient row */
    mv t4, t3
    esp.vld.128.ip q2, t4, 16
    esp.mov.s16.qacc q4
    MAC8 q2, q3, t4, q1
    esp.srcmb.s16.qacc q6, s8, 0
    esp.vst.128.ip q6, s9, 16
    j 3f
2:
    addi a2, a2, 16
    esp.vst.128.ip q5, s9, 16
3:
    srli s11, s11, 1
    addi s10, s10, -1
    bnez s10, 1b

    addi s9, t3, 128
    addi t4, t3, 272
    esp.vld.128.ip q4, t4, 0                /* q4 = 16384 */
    addi t4, t3, 288
    esp.vld.128.ip q5, t4, 0                /* q5 lane 0 = 514 */
    li s8, 16
    li s0, 0
    li s1, 255
    li s10, 4
4:
    COL_LINE q6
    COL_LINE q7
    esp.vsat.s16 q6, q6, s0, s1
    esp.vsat.s16 q7, q7, s0, s1
    esp.vunzip.8 q6, q7
    esp.vst.l.64.xp q6, a0, a1
    esp.vst.h.64.xp q6, a0, a1
    addi s10, s10, -1
    bnez s10, 4b

    lw s0, 128(sp)
    lw s1, 132(sp)
    lw s8, 136(sp)
    lw s9, 140(sp)
    lw s10, 144(sp)
    lw s11, 148(sp)
    addi sp, sp, 160
    ret
    .size imgf_k_idct, .-imgf_k_idct

    .section .rodata
    .balign 16
imgf_pack565_consts:
    .short 0x00F8, 0x00F8, 0x00F8, 0x00F8, 0x00F8, 0x00F8, 0x00F8, 0x00F8
    .short 0x00FC, 0x00FC, 0x00FC, 0x00FC, 0x00FC, 0x00FC, 0x00FC, 0x00FC
    .short -3, -3, -3, -3, -3, -3, -3, -3
    .short 8, 8, 8, 8, 8, 8, 8, 8
    .short 3, 3, 3, 3, 3, 3, 3, 3

    .text

/* void imgf_k_pack_bytes(uint8_t *dst, const uint16_t *src, int n16)
 *
 * n16 groups of 16 samples, each 0..255 in a 16-bit lane, to bytes. */
    .balign 4
    .global imgf_k_pack_bytes
    .type imgf_k_pack_bytes, @function
imgf_k_pack_bytes:
    addi sp, sp, -16
    sw s0, 0(sp)
    sw s1, 4(sp)
    li s0, 0
    li s1, 255
    blez a2, 2f
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    esp.vsat.s16 q0, q0, s0, s1
    esp.vsat.s16 q1, q1, s0, s1
    esp.vunzip.8 q0, q1
    esp.vst.128.ip q0, a0, 16
    addi a2, a2, -1
    bnez a2, 1b
2:
    lw s0, 0(sp)
    lw s1, 4(sp)
    addi sp, sp, 16
    ret
    .size imgf_k_pack_bytes, .-imgf_k_pack_bytes

/* void imgf_k_pack_rgb565(uint8_t *dst, const uint16_t *src, int n8)
 *
 * n8 groups of 8 R,G,B triplets to host-endian RGB565. */
    .balign 4
    .global imgf_k_pack_rgb565
    .type imgf_k_pack_rgb565, @function
imgf_k_pack_rgb565:
    addi sp, sp, -16
    sw s0, 0(sp)
    sw s1, 4(sp)
    la t3, imgf_pack565_consts
    esp.vld.128.ip q3, t3, 16               /* 0x00F8 */
    esp.vld.128.ip q4, t3, 16               /* 0x00FC */
    esp.vld.128.ip q5, t3, 16               /* shift -3 */
    esp.vld.128.ip q6, t3, 16               /* shift +8 */
    esp.vld.128.ip q7, t3, 16               /* shift +3 */
    li s0, 0
    li s1, 255
    blez a2, 2f
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    esp.vld.128.ip q2, a1, 16
    esp.vsat.s16 q0, q0, s0, s1
    esp.vsat.s16 q1, q1, s0, s1
    esp.vsat.s16 q2, q2, s0, s1
    esp.vunzipt.16 q0, q1, q2               /* q0 = R, q1 = G, q2 = B */
    esp.andq q0, q0, q3
    esp.vsld.16 q0, q0, q6
    esp.andq q1, q1, q4
    esp.vsld.16 q1, q1, q7
    esp.vsld.16 q2, q2, q5
    esp.orq q0, q0, q1
    esp.orq q0, q0, q2
    esp.vst.128.ip q0, a0, 16
    addi a2, a2, -1
    bnez a2, 1b
2:
    lw s0, 0(sp)
    lw s1, 4(sp)
    addi sp, sp, 16
    ret
    .size imgf_k_pack_rgb565, .-imgf_k_pack_rgb565

/* CFG is global and not saved per task, so every entry point into PIE code
 * sets the misaligned load/store bits itself. SAR feeds that same path and
 * must be left at 0. */
    .balign 4
    .global imgf_k_prepare
    .type imgf_k_prepare, @function
imgf_k_prepare:
    esp.movx.r.cfg t6
    ori t6, t6, 3
    esp.movx.w.cfg t6
    li t6, 0
    esp.movx.w.sar t6
    ret
    .size imgf_k_prepare, .-imgf_k_prepare
