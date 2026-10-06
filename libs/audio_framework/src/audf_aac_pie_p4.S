/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

    .text

/* CFG is global and not saved per task, so every entry point into PIE code
 * sets the misaligned load/store bits itself. SAR feeds that same path and
 * must be left at 0. */
    .balign 4
    .global aac_k_prepare
    .type aac_k_prepare, @function
aac_k_prepare:
    esp.movx.r.cfg t6
    ori t6, t6, 3
    esp.movx.w.cfg t6
    li t6, 0
    esp.movx.w.sar t6
    ret
    .size aac_k_prepare, .-aac_k_prepare

/* q6/q7 += sum of the 32-bit products of q0 (8 x s16 from t3) and q1 (from t4) */
.macro MAC32 dstep, cstep
    esp.vld.128.ip q0, t3, \dstep
    esp.vld.128.ip q1, t4, \cstep
    esp.vmul.s32.s16xs16 q2, q3, q0, q1
    esp.vadd.s32 q6, q6, q2
    esp.vadd.s32 q7, q7, q3
.endm

/* Every kernel that stores ends with a fence: without it a scalar load right
 * after the call can still see the old contents (seen on this chip revision as
 * non-deterministic output). */

/* void aac_k_qmf_analysis(int32_t *u, const int16_t *hi, const int16_t *lo, const int16_t *c) */
    .balign 4
    .global aac_k_qmf_analysis
    .type aac_k_qmf_analysis, @function
aac_k_qmf_analysis:
    li t5, 8
    li a4, 16
1:
    esp.zero.q q6
    esp.zero.q q7
    li t6, 0
    esp.movx.w.sar t6
    mv t3, a1
    mv t4, a3
    MAC32 -128, 128
    MAC32 -128, 128
    MAC32 -128, 128
    MAC32 -128, 128
    MAC32 -128, 128
    esp.movx.w.sar a4
    mv t3, a2
    mv t4, a3
    MAC32 -128, 128
    MAC32 -128, 128
    MAC32 -128, 128
    MAC32 -128, 128
    MAC32 -128, 128
    esp.vst.128.ip q6, a0, 16
    esp.vst.128.ip q7, a0, 16
    addi a1, a1, 16
    addi a2, a2, 16
    addi a3, a3, 16
    addi t5, t5, -1
    bnez t5, 1b
    li t6, 0
    esp.movx.w.sar t6
    fence
    ret
    .size aac_k_qmf_analysis, .-aac_k_qmf_analysis

/* void aac_k_qmf_synthesis(int32_t *acc, const int16_t *hi, const int16_t *lo, const int16_t *c) */
    .balign 4
    .global aac_k_qmf_synthesis
    .type aac_k_qmf_synthesis, @function
aac_k_qmf_synthesis:
    li t5, 8
    li a4, 16
1:
    esp.zero.q q6
    esp.zero.q q7
    li t6, 0
    esp.movx.w.sar t6
    mv t3, a1
    mv t4, a3
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    esp.movx.w.sar a4
    mv t3, a2
    mv t4, a3
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    MAC32 384, 128
    MAC32 128, 128
    esp.vst.128.ip q6, a0, 16
    esp.vst.128.ip q7, a0, 16
    addi a1, a1, 16
    addi a2, a2, 16
    addi a3, a3, 16
    addi t5, t5, -1
    bnez t5, 1b
    li t6, 0
    esp.movx.w.sar t6
    fence
    ret
    .size aac_k_qmf_synthesis, .-aac_k_qmf_synthesis

/* q0 = 8 x s16 of (8 x s32 at t3 + bias q5) >> SAR, saturated; t3 += 32 */
.macro PACK8
    esp.vld.128.ip q0, t3, 16
    esp.vld.128.ip q1, t3, 16
    esp.vadd.s32 q0, q0, q5
    esp.vadd.s32 q1, q1, q5
    esp.vsr.s32 q0, q0
    esp.vsr.s32 q1, q1
    esp.vsat.s32 q0, q0, t5, t6
    esp.vsat.s32 q1, q1, t5, t6
    esp.vunzip.16 q0, q1
.endm

/* void aac_k_pack(int16_t *out, const int32_t *a, const int32_t *b, int n, int shift) */
    .balign 4
    .global aac_k_pack
    .type aac_k_pack, @function
aac_k_pack:
    addi sp, sp, -16
    li t4, 1
    addi t3, a4, -1
    sll t4, t4, t3
    sw t4, 0(sp)
    mv a5, sp
    esp.vldbc.32.ip q5, a5, 0
    esp.movx.w.sar a4
    li t5, -32768
    li t6, 32767
    srli a3, a3, 3
    beqz a2, 2f
1:
    mv t3, a1
    PACK8
    esp.vst.128.ip q0, a5, 0
    mv a1, t3
    mv t3, a2
    PACK8
    mv a2, t3
    esp.vld.128.ip q2, a5, 0
    esp.vzip.16 q2, q0
    esp.vst.128.ip q2, a0, 16
    esp.vst.128.ip q0, a0, 16
    addi a3, a3, -1
    bnez a3, 1b
    j 3f
2:
    mv t3, a1
    PACK8
    mv a1, t3
    esp.vst.128.ip q0, a0, 16
    addi a3, a3, -1
    bnez a3, 2b
3:
    li t6, 0
    esp.movx.w.sar t6
    addi sp, sp, 16
    fence
    ret
    .size aac_k_pack, .-aac_k_pack

    .section .rodata
    .balign 16
aac_k_half_lsb:
    .word 0x8000, 0x8000, 0x8000, 0x8000
aac_k_bitrev5:
    .byte 0, 16, 8, 24, 4, 20, 12, 28, 2, 18, 10, 26, 6, 22, 14, 30
    .byte 1, 17, 9, 25, 5, 21, 13, 29, 3, 19, 11, 27, 7, 23, 15, 31
    .text

/* (q5, q6) = (q0 + i q1) * w for four lanes, w at \tp as [wr x4, wi x4]; each
 * product is (x * w) >> 16 from the 16-bit halves of x. Needs q7 = 0x8000 x4,
 * a4 = 0, a5 = 16; clobbers q0-q4, t6 and SAR. */
.macro CMUL tp
    esp.orq q2, q0, q0
    esp.orq q3, q1, q1
    esp.vadd.s32 q2, q2, q7
    esp.vadd.s32 q3, q3, q7
    esp.vunzip.16 q2, q3
    esp.vunzip.16 q0, q1
    mv t6, \tp
    esp.vld.128.ip q4, t6, 0
    esp.movx.w.sar a4
    esp.vmul.s32.s16xs16 q1, q2, q3, q4
    esp.vsub.s32 q5, q1, q2
    esp.movx.w.sar a5
    esp.vmul.s32.s16xs16 q1, q2, q0, q4
    esp.vadd.s32 q5, q5, q1
    esp.vsub.s32 q5, q5, q2
    esp.vld.h.64.ip q4, t6, 8
    esp.vld.l.64.ip q4, t6, 0
    esp.movx.w.sar a4
    esp.vmul.s32.s16xs16 q1, q2, q3, q4
    esp.vadd.s32 q6, q1, q2
    esp.movx.w.sar a5
    esp.vmul.s32.s16xs16 q1, q2, q0, q4
    esp.vadd.s32 q6, q6, q1
    esp.vadd.s32 q6, q6, q2
.endm

/* Radix-2 DIF butterfly halving both outputs: a at t3/t5 (re/im), c at t4/s0.
 * Leaves the difference in q0/q1 with SAR = 1 and advances the a pointers. */
.macro BF_SUM
    esp.vld.128.ip q0, t3, 0
    esp.vld.128.ip q1, t5, 0
    esp.vld.128.ip q2, t4, 0
    esp.vld.128.ip q3, s0, 0
    esp.vadd.s32 q4, q0, q2
    esp.vadd.s32 q5, q1, q3
    esp.vsub.s32 q0, q0, q2
    esp.vsub.s32 q1, q1, q3
    esp.movx.w.sar s8
    esp.vsr.s32 q4, q4
    esp.vsr.s32 q5, q5
    esp.vst.128.ip q4, t3, 16
    esp.vst.128.ip q5, t5, 16
.endm

.macro BF_ONE
    BF_SUM
    esp.vsr.s32 q0, q0
    esp.vsr.s32 q1, q1
    esp.vst.128.ip q0, t4, 16
    esp.vst.128.ip q1, s0, 16
.endm

.macro BF_NEG_I
    BF_SUM
    esp.zero.q q2
    esp.vsub.s32 q2, q2, q0
    esp.vsr.s32 q1, q1
    esp.vsr.s32 q2, q2
    esp.vst.128.ip q1, t4, 16
    esp.vst.128.ip q2, s0, 16
.endm

.macro BF_TW
    BF_SUM
    CMUL s1
    esp.vst.128.ip q5, t4, 16
    esp.vst.128.ip q6, s0, 16
.endm

/* void aac_k_dct4_64x4(int32_t *x, int32_t *tmp) */
    .balign 4
    .global aac_k_dct4_64x4
    .type aac_k_dct4_64x4, @function
aac_k_dct4_64x4:
    addi sp, sp, -32
    sw s0, 0(sp)
    sw s1, 4(sp)
    sw s8, 8(sp)
    sw s9, 12(sp)
    sw s10, 16(sp)
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    li a4, 0
    li a5, 16
    li s8, 1
    addi a2, a1, 512

    la a3, aac_pie_dct64_pre
    mv t3, a0
    addi t4, a0, 63 * 16
    mv t5, a1
    mv s0, a2
    li t0, 32
1:
    esp.vld.128.ip q0, t3, 32
    esp.vld.128.ip q1, t4, -32
    CMUL a3
    addi a3, a3, 16
    esp.vst.128.ip q5, t5, 16
    esp.vst.128.ip q6, s0, 16
    addi t0, t0, -1
    bnez t0, 1b

    la a3, aac_pie_fft32
    li t0, 16
    li t1, 16
2:
    slli t2, t0, 4
    mv s9, a1
3:
    mv t3, s9
    add t4, s9, t2
    addi t5, t3, 512
    addi s0, t4, 512
    BF_ONE
    add s1, a3, t1
    addi s10, t0, -1
4:
    BF_TW
    add s1, s1, t1
    addi s10, s10, -1
    bnez s10, 4b
    add s9, s9, t2
    add s9, s9, t2
    bne s9, a2, 3b
    srli t0, t0, 1
    slli t1, t1, 1
    li t2, 2
    bgt t0, t2, 2b

    mv s9, a1
5:
    mv t3, s9
    addi t4, s9, 32
    addi t5, t3, 512
    addi s0, t4, 512
    BF_ONE
    BF_NEG_I
    addi s9, s9, 64
    bne s9, a2, 5b

    mv s9, a1
6:
    mv t3, s9
    addi t4, s9, 16
    addi t5, t3, 512
    addi s0, t4, 512
    BF_ONE
    addi s9, s9, 32
    bne s9, a2, 6b

    esp.vld.128.ip q0, a1, 0
    esp.vld.128.ip q1, a2, 0
    esp.movx.w.sar s8
    esp.vsr.s32 q0, q0
    esp.vsr.s32 q1, q1
    mv t3, a0
    esp.vst.128.ip q0, t3, 0
    esp.zero.q q2
    esp.vsub.s32 q2, q2, q1
    addi t4, a0, 63 * 16
    esp.vst.128.ip q2, t4, 0
    la a3, aac_pie_dct64_post + 16
    la t1, aac_k_bitrev5 + 1
    addi t3, a0, 32
    addi t4, a0, 61 * 16
    li t0, 31
7:
    lbu t2, 0(t1)
    slli t2, t2, 4
    add t5, a1, t2
    add s0, a2, t2
    esp.vld.128.ip q0, t5, 0
    esp.vld.128.ip q1, s0, 0
    CMUL a3
    esp.vst.128.ip q5, t3, 32
    esp.zero.q q0
    esp.vsub.s32 q0, q0, q6
    esp.vst.128.ip q0, t4, -32
    addi a3, a3, 16
    addi t1, t1, 1
    addi t0, t0, -1
    bnez t0, 7b

    esp.movx.w.sar a4
    lw s0, 0(sp)
    lw s1, 4(sp)
    lw s8, 8(sp)
    lw s9, 12(sp)
    lw s10, 16(sp)
    addi sp, sp, 32
    fence
    ret
    .size aac_k_dct4_64x4, .-aac_k_dct4_64x4

/* Transposes the 4x4 block in \a, \b, \c, \d. */
.macro TRANSPOSE4 a, b, c, d
    esp.vzip.32 \a, \c
    esp.vzip.32 \b, \d
    esp.vzip.32 \a, \b
    esp.vzip.32 \c, \d
.endm

/* Loads slots p, p + 64, p + 32, p + 96 at \off from t3, transposes them and
 * stores lane l back to the slot lane l came from. */
.macro CROSS_TRANSPOSE off
    add s10, t3, \off
    addi t4, s10, 512
    addi t5, s10, 1024
    addi s0, s10, 1536
    esp.vld.128.ip q0, s10, 0
    esp.vld.128.ip q1, t5, 0
    esp.vld.128.ip q2, t4, 0
    esp.vld.128.ip q3, s0, 0
    TRANSPOSE4 q0, q1, q2, q3
    esp.vst.128.ip q0, s10, 0
    esp.vst.128.ip q1, t5, 0
    esp.vst.128.ip q2, t4, 0
    esp.vst.128.ip q3, s0, 0
.endm

/* Rotates the slot at \slot bytes from t3 by the next twiddle at a3. */
.macro CROSS_TWIDDLE slot
    addi t4, t3, \slot
    add t5, t4, s11
    esp.vld.128.ip q0, t4, 0
    esp.vld.128.ip q1, t5, 0
    CMUL a3
    addi a3, a3, 16
    esp.vst.128.ip q5, t4, 0
    esp.vst.128.ip q6, t5, 0
.endm

/* void aac_k_dct4_1024(int32_t *x, int32_t *tmp); see aac_k_dct4_1024_c. */
    .balign 4
    .global aac_k_dct4_1024
    .type aac_k_dct4_1024, @function
aac_k_dct4_1024:
    addi sp, sp, -32
    sw s0, 0(sp)
    sw s1, 4(sp)
    sw s8, 8(sp)
    sw s9, 12(sp)
    sw s10, 16(sp)
    sw s11, 20(sp)
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    li a4, 0
    li a5, 16
    li s8, 1
    li s11, 2048
    add a2, a1, s11

    la a3, aac_pie_dct1024_pre
    mv t3, a0
    add t4, a0, s11
    add t4, t4, s11
    addi t4, t4, -8
    mv t5, a1
    mv s0, a2
    li t0, 128
1:
    esp.vld.128.ip q0, t3, 16
    esp.vld.128.ip q1, t3, 16
    esp.vunzip.32 q0, q1
    esp.vld.l.64.ip q2, t4, -8
    esp.vld.h.64.ip q2, t4, -8
    esp.vld.l.64.ip q3, t4, -8
    esp.vld.h.64.ip q3, t4, -8
    esp.vunzip.32 q2, q3
    esp.orq q1, q3, q3
    CMUL a3
    addi a3, a3, 16
    esp.vst.128.ip q5, t5, 16
    esp.vst.128.ip q6, s0, 16
    addi t0, t0, -1
    bnez t0, 1b

    la a3, aac_pie_dct1024_fft128
    li t0, 64
    li t1, 16
2:
    slli t2, t0, 4
    mv s9, a1
3:
    mv t3, s9
    add t4, s9, t2
    add t5, t3, s11
    add s0, t4, s11
    BF_ONE
    add s1, a3, t1
    addi s10, t0, -1
4:
    BF_TW
    add s1, s1, t1
    addi s10, s10, -1
    bnez s10, 4b
    add s9, s9, t2
    add s9, s9, t2
    bne s9, a2, 3b
    srli t0, t0, 1
    slli t1, t1, 1
    li t2, 2
    bgt t0, t2, 2b

    mv s9, a1
5:
    mv t3, s9
    addi t4, s9, 32
    add t5, t3, s11
    add s0, t4, s11
    BF_ONE
    BF_NEG_I
    addi s9, s9, 64
    bne s9, a2, 5b

    mv s9, a1
6:
    mv t3, s9
    addi t4, s9, 16
    add t5, t3, s11
    add s0, t4, s11
    BF_ONE
    addi s9, s9, 32
    bne s9, a2, 6b

    la a3, aac_pie_dct1024_mid
    mv t3, a1
    addi s9, a1, 512
7:
    CROSS_TRANSPOSE zero
    CROSS_TRANSPOSE s11
    add t4, t3, s11
    esp.vld.128.ip q0, t3, 0
    esp.vld.128.ip q1, t4, 0
    esp.movx.w.sar s8
    esp.vsr.s32 q0, q0
    esp.vsr.s32 q1, q1
    esp.vst.128.ip q0, t3, 0
    esp.vst.128.ip q1, t4, 0
    CROSS_TWIDDLE 1024
    CROSS_TWIDDLE 512
    CROSS_TWIDDLE 1536

    addi t4, t3, 512
    add t5, t3, s11
    add s0, t4, s11
    esp.vld.128.ip q0, t3, 0
    esp.vld.128.ip q1, t5, 0
    esp.vld.128.ip q2, t4, 0
    esp.vld.128.ip q3, s0, 0
    esp.vadd.s32 q4, q0, q2
    esp.vadd.s32 q5, q1, q3
    esp.vsub.s32 q0, q0, q2
    esp.vsub.s32 q1, q1, q3
    esp.vst.128.ip q4, t3, 0
    esp.vst.128.ip q5, t5, 0
    esp.vst.128.ip q0, t4, 0
    esp.vst.128.ip q1, s0, 0
    addi s10, t3, 1024
    addi t4, t3, 1536
    add t5, s10, s11
    add s0, t4, s11
    esp.vld.128.ip q0, s10, 0
    esp.vld.128.ip q1, t5, 0
    esp.vld.128.ip q2, t4, 0
    esp.vld.128.ip q3, s0, 0
    esp.vadd.s32 q4, q0, q2
    esp.vadd.s32 q5, q1, q3
    esp.vsub.s32 q0, q0, q2
    esp.vsub.s32 q1, q1, q3
    esp.zero.q q6
    esp.vsub.s32 q6, q6, q0
    esp.vst.128.ip q4, s10, 0
    esp.vst.128.ip q5, t5, 0
    esp.vst.128.ip q1, t4, 0
    esp.vst.128.ip q6, s0, 0

    mv s1, t3
    addi t4, t3, 1024
    add t5, t3, s11
    add s0, t4, s11
    BF_ONE
    addi t3, s1, 512
    addi t4, t3, 1024
    add t5, t3, s11
    add s0, t4, s11
    BF_ONE
    addi t3, s1, 16
    bne t3, s9, 7b

    la a3, aac_pie_dct1024_post
    la s1, aac_k_bitrev5
    mv s9, a0
    add s10, a0, s11
    mv t3, a1
    li t0, 0
8:
    add t5, t3, s11
    esp.vld.128.ip q0, t3, 16
    esp.vld.128.ip q1, t5, 0
    CMUL a3
    addi a3, a3, 16
    andi t1, t0, 31
    add t1, t1, s1
    lbu t1, 0(t1)
    andi t2, t0, 96
    add t1, t1, t2
    slli t1, t1, 4
    add t4, s9, t1
    esp.vst.128.ip q5, t4, 0
    esp.zero.q q0
    esp.vsub.s32 q0, q0, q6
    add t4, s10, t1
    esp.vst.128.ip q0, t4, 0
    addi t0, t0, 1
    li t1, 128
    bne t0, t1, 8b

    fence
    lw t1, 0(a1)
    srai t1, t1, 1
    sw t1, 0(s9)
    lw t1, 0(a2)
    srai t1, t1, 1
    neg t1, t1
    sw t1, 0(s10)

    esp.movx.w.sar a4
    lw s0, 0(sp)
    lw s1, 4(sp)
    lw s8, 8(sp)
    lw s9, 12(sp)
    lw s10, 16(sp)
    lw s11, 20(sp)
    addi sp, sp, 32
    fence
    ret
    .size aac_k_dct4_1024, .-aac_k_dct4_1024

/* (q5, q6) += (q0 + i q1) * w, as CMUL. */
.macro CMUL_ACC tp
    esp.orq q2, q0, q0
    esp.orq q3, q1, q1
    esp.vadd.s32 q2, q2, q7
    esp.vadd.s32 q3, q3, q7
    esp.vunzip.16 q2, q3
    esp.vunzip.16 q0, q1
    mv t6, \tp
    esp.vld.128.ip q4, t6, 0
    esp.movx.w.sar a4
    esp.vmul.s32.s16xs16 q1, q2, q3, q4
    esp.vadd.s32 q5, q5, q1
    esp.vsub.s32 q5, q5, q2
    esp.movx.w.sar a5
    esp.vmul.s32.s16xs16 q1, q2, q0, q4
    esp.vadd.s32 q5, q5, q1
    esp.vsub.s32 q5, q5, q2
    esp.vld.h.64.ip q4, t6, 8
    esp.vld.l.64.ip q4, t6, 0
    esp.movx.w.sar a4
    esp.vmul.s32.s16xs16 q1, q2, q3, q4
    esp.vadd.s32 q6, q6, q1
    esp.vadd.s32 q6, q6, q2
    esp.movx.w.sar a5
    esp.vmul.s32.s16xs16 q1, q2, q0, q4
    esp.vadd.s32 q6, q6, q1
    esp.vadd.s32 q6, q6, q2
.endm

/* (q5, q6) \op= (q0 * c, q1 * c) with c at \tp as [c x4, c x4]; same needs and
 * clobbers as CMUL. */
.macro RMUL_ACC tp, op
    esp.orq q2, q0, q0
    esp.orq q3, q1, q1
    esp.vadd.s32 q2, q2, q7
    esp.vadd.s32 q3, q3, q7
    esp.vunzip.16 q2, q3
    esp.vunzip.16 q0, q1
    mv t6, \tp
    esp.vld.128.ip q4, t6, 0
    esp.movx.w.sar a4
    esp.vmul.s32.s16xs16 q1, q2, q3, q4
    \op q5, q5, q1
    \op q6, q6, q2
    esp.movx.w.sar a5
    esp.vmul.s32.s16xs16 q1, q2, q0, q4
    \op q5, q5, q1
    \op q6, q6, q2
.endm

.macro DOUBLE
    esp.vadd.s32 q5, q5, q5
    esp.vadd.s32 q6, q6, q6
.endm

/* One all-pass link for a group: \link is the delayed state, the link input
 * waits at \dst, which then takes the new state; the output goes to \next
 * (advanced by \nadv). t3 walks the [q, ag] coefficients. */
.macro AP_LINK link, dst, next, nadv
    esp.vld.128.ip q0, \link, 16
    esp.vld.128.ip q1, \link, 16
    CMUL t3
    addi t3, t3, 16
    esp.vld.128.ip q0, \dst, 16
    esp.vld.128.ip q1, \dst, -16
    RMUL_ACC t3, esp.vsub.s32
    DOUBLE
    esp.vst.128.ip q5, \next, 16
    esp.vst.128.ip q6, \next, \nadv
    esp.orq q0, q5, q5
    esp.orq q1, q6, q6
    esp.zero.q q5
    esp.zero.q q6
    RMUL_ACC t3, esp.vadd.s32
    addi t3, t3, 16
    DOUBLE
    esp.vld.128.ip q0, \dst, 16
    esp.vld.128.ip q1, \dst, -16
    esp.vadd.s32 q5, q5, q0
    esp.vadd.s32 q6, q6, q1
    esp.vst.128.ip q5, \dst, 16
    esp.vst.128.ip q6, \dst, 16
.endm

/* void aac_k_ps_allpass(int32_t *r, const int32_t *s, int32_t *ap, int w); see aac_k_ps_allpass_c. */
    .balign 4
    .global aac_k_ps_allpass
    .type aac_k_ps_allpass, @function
aac_k_ps_allpass:
    addi sp, sp, -32
    sw s0, 0(sp)
    sw s1, 4(sp)
    sw s8, 8(sp)
    sw s9, 12(sp)
    sw s10, 16(sp)
    sw s11, 20(sp)
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    li a4, 0
    li a5, 16
    li t2, 2048
    slli t1, a3, 8
    add s9, a2, t1
    add s10, s9, t2
    add s11, s10, t2
    addi t1, a3, -3
    andi t1, t1, 7
    slli t1, t1, 8
    add s0, a2, t1
    addi t1, a3, -4
    andi t1, t1, 7
    slli t1, t1, 8
    add t1, a2, t1
    add s1, t1, t2
    addi t1, a3, -5
    andi t1, t1, 7
    slli t1, t1, 8
    add t1, a2, t1
    add t1, t1, t2
    add s8, t1, t2
    la t3, aac_pie_ps_ap
    li t0, 8
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    CMUL t3
    addi t3, t3, 16
    DOUBLE
    esp.vst.128.ip q5, s9, 16
    esp.vst.128.ip q6, s9, -16
    AP_LINK s0, s9, s10, -16
    AP_LINK s1, s10, s11, -16
    AP_LINK s8, s11, a0, 16
    addi t0, t0, -1
    bnez t0, 1b
    esp.movx.w.sar a4
    lw s0, 0(sp)
    lw s1, 4(sp)
    lw s8, 8(sp)
    lw s9, 12(sp)
    lw s10, 16(sp)
    lw s11, 20(sp)
    addi sp, sp, 32
    fence
    ret
    .size aac_k_ps_allpass, .-aac_k_ps_allpass

/* void aac_k_ps_gain(int32_t *r, const int16_t *g, int groups); see aac_k_ps_gain_c. */
    .balign 4
    .global aac_k_ps_gain
    .type aac_k_ps_gain, @function
aac_k_ps_gain:
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    li a4, 0
    li a5, 16
1:
    esp.vld.128.ip q0, a0, 16
    esp.vld.128.ip q1, a0, -16
    esp.zero.q q5
    esp.zero.q q6
    RMUL_ACC a1, esp.vadd.s32
    addi a1, a1, 16
    DOUBLE
    esp.vst.128.ip q5, a0, 16
    esp.vst.128.ip q6, a0, 16
    addi a2, a2, -1
    bnez a2, 1b
    esp.movx.w.sar a4
    fence
    ret
    .size aac_k_ps_gain, .-aac_k_ps_gain

/* Steps one [re x4, im x4] Q30 pair at a2 by a3 and stores its rounded upper
 * halves at t5 as a coefficient vector; advances a2, a3 and t5. */
.macro PS_COEF
    esp.vld.128.ip q0, a2, 16
    esp.vld.128.ip q1, a2, -16
    esp.vld.128.ip q2, a3, 16
    esp.vld.128.ip q3, a3, 16
    esp.vadd.s32 q0, q0, q2
    esp.vadd.s32 q1, q1, q3
    esp.vst.128.ip q0, a2, 16
    esp.vst.128.ip q1, a2, 16
    esp.vadd.s32 q0, q0, q7
    esp.vadd.s32 q1, q1, q7
    esp.vunzip.16 q0, q1
    esp.vst.128.ip q1, t5, 16
.endm

/* void aac_k_ps_mix(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups); see aac_k_ps_mix_c. */
    .balign 4
    .global aac_k_ps_mix
    .type aac_k_ps_mix, @function
aac_k_ps_mix:
    addi sp, sp, -96
    mv t0, a4
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    li a4, 0
    li a5, 16
1:
    addi t5, sp, 32
    PS_COEF
    PS_COEF
    PS_COEF
    PS_COEF
    addi t5, sp, 32
    esp.zero.q q5
    esp.zero.q q6
    esp.vld.128.ip q0, a0, 16
    esp.vld.128.ip q1, a0, -16
    CMUL_ACC t5
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, -16
    addi t4, t5, 16
    CMUL_ACC t4
    DOUBLE
    DOUBLE
    mv t4, sp
    esp.vst.128.ip q5, t4, 16
    esp.vst.128.ip q6, t4, 0
    esp.zero.q q5
    esp.zero.q q6
    esp.vld.128.ip q0, a0, 16
    esp.vld.128.ip q1, a0, -16
    addi t4, t5, 32
    CMUL_ACC t4
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, -16
    addi t4, t5, 48
    CMUL_ACC t4
    DOUBLE
    DOUBLE
    esp.vst.128.ip q5, a1, 16
    esp.vst.128.ip q6, a1, 16
    mv t4, sp
    esp.vld.128.ip q0, t4, 16
    esp.vld.128.ip q1, t4, 0
    esp.vst.128.ip q0, a0, 16
    esp.vst.128.ip q1, a0, 16
    addi t0, t0, -1
    bnez t0, 1b
    esp.movx.w.sar a4
    addi sp, sp, 96
    fence
    ret
    .size aac_k_ps_mix, .-aac_k_ps_mix

/* Steps the re half of a Q30 pair at a2 by a3 and stores its rounded upper
 * halves at t5 as [c x4, c x4]; advances a2, a3 past the pair and t5. */
.macro PS_COEF_REAL
    esp.vld.128.ip q0, a2, 0
    esp.vld.128.ip q2, a3, 32
    esp.vadd.s32 q0, q0, q2
    esp.vst.128.ip q0, a2, 32
    esp.vadd.s32 q0, q0, q7
    esp.orq q1, q0, q0
    esp.vunzip.16 q0, q1
    esp.vst.128.ip q1, t5, 16
.endm

/* void aac_k_ps_mix_real(int32_t *l, int32_t *r, int32_t *acc, const int32_t *step, int groups);
 * see aac_k_ps_mix_real_c. */
    .balign 4
    .global aac_k_ps_mix_real
    .type aac_k_ps_mix_real, @function
aac_k_ps_mix_real:
    addi sp, sp, -96
    mv t0, a4
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    li a4, 0
    li a5, 16
1:
    addi t5, sp, 32
    PS_COEF_REAL
    PS_COEF_REAL
    PS_COEF_REAL
    PS_COEF_REAL
    addi t5, sp, 32
    esp.zero.q q5
    esp.zero.q q6
    esp.vld.128.ip q0, a0, 16
    esp.vld.128.ip q1, a0, -16
    RMUL_ACC t5, esp.vadd.s32
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, -16
    addi t4, t5, 16
    RMUL_ACC t4, esp.vadd.s32
    DOUBLE
    DOUBLE
    mv t4, sp
    esp.vst.128.ip q5, t4, 16
    esp.vst.128.ip q6, t4, 0
    esp.zero.q q5
    esp.zero.q q6
    esp.vld.128.ip q0, a0, 16
    esp.vld.128.ip q1, a0, -16
    addi t4, t5, 32
    RMUL_ACC t4, esp.vadd.s32
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, -16
    addi t4, t5, 48
    RMUL_ACC t4, esp.vadd.s32
    DOUBLE
    DOUBLE
    esp.vst.128.ip q5, a1, 16
    esp.vst.128.ip q6, a1, 16
    mv t4, sp
    esp.vld.128.ip q0, t4, 16
    esp.vld.128.ip q1, t4, 0
    esp.vst.128.ip q0, a0, 16
    esp.vst.128.ip q1, a0, 16
    addi t0, t0, -1
    bnez t0, 1b
    esp.movx.w.sar a4
    addi sp, sp, 96
    fence
    ret
    .size aac_k_ps_mix_real, .-aac_k_ps_mix_real

/* void aac_k_ps_unzip(int32_t *dst, const int32_t *src, int groups); see aac_k_ps_unzip_c. */
    .balign 4
    .global aac_k_ps_unzip
    .type aac_k_ps_unzip, @function
aac_k_ps_unzip:
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    esp.vunzip.32 q0, q1
    esp.vst.128.ip q0, a0, 16
    esp.vst.128.ip q1, a0, 16
    addi a2, a2, -1
    bnez a2, 1b
    fence
    ret
    .size aac_k_ps_unzip, .-aac_k_ps_unzip

/* void aac_k_ps_zip(int32_t *dst, const int32_t *src, int groups); see aac_k_ps_zip_c. */
    .balign 4
    .global aac_k_ps_zip
    .type aac_k_ps_zip, @function
aac_k_ps_zip:
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    esp.vzip.32 q0, q1
    esp.vst.128.ip q0, a0, 16
    esp.vst.128.ip q1, a0, 16
    addi a2, a2, -1
    bnez a2, 1b
    fence
    ret
    .size aac_k_ps_zip, .-aac_k_ps_zip

/* void aac_k_syn_in4(int32_t *a, int32_t *b, const int32_t *const x[4]); see aac_k_syn_in4_c. */
    .balign 4
    .global aac_k_syn_in4
    .type aac_k_syn_in4, @function
aac_k_syn_in4:
    lw t3, 0(a2)
    lw t4, 4(a2)
    lw t5, 8(a2)
    lw t6, 12(a2)
    addi a1, a1, 63 * 16
    li a3, 4
    esp.movx.w.sar a3
    li a4, 16
1:
    esp.vld.128.ip q0, t3, 16
    esp.vld.128.ip q1, t3, 16
    esp.vunzip.32 q0, q1
    esp.vld.128.ip q2, t4, 16
    esp.vld.128.ip q3, t4, 16
    esp.vunzip.32 q2, q3
    esp.vld.128.ip q4, t5, 16
    esp.vld.128.ip q5, t5, 16
    esp.vunzip.32 q4, q5
    esp.vld.128.ip q6, t6, 16
    esp.vld.128.ip q7, t6, 16
    esp.vunzip.32 q6, q7
    /* q0/q2/q4/q6 = re of rows 0..3, q1/q3/q5/q7 = im */
    esp.vzip.32 q0, q4
    esp.vzip.32 q2, q6
    esp.vzip.32 q0, q2
    esp.vzip.32 q4, q6
    esp.vsl.32 q0, q0
    esp.vsl.32 q2, q2
    esp.vsl.32 q4, q4
    esp.vsl.32 q6, q6
    esp.vst.128.ip q0, a0, 16
    esp.vst.128.ip q2, a0, 16
    esp.vst.128.ip q4, a0, 16
    esp.vst.128.ip q6, a0, 16
    esp.vzip.32 q1, q5
    esp.vzip.32 q3, q7
    esp.vzip.32 q1, q3
    esp.vzip.32 q5, q7
    esp.vsl.32 q1, q1
    esp.vsl.32 q3, q3
    esp.vsl.32 q5, q5
    esp.vsl.32 q7, q7
    esp.vst.128.ip q1, a1, -16
    esp.vst.128.ip q3, a1, -16
    esp.vst.128.ip q5, a1, -16
    esp.vst.128.ip q7, a1, -16
    addi a4, a4, -1
    bnez a4, 1b
    li a3, 0
    esp.movx.w.sar a3
    fence
    ret
    .size aac_k_syn_in4, .-aac_k_syn_in4

/* Stores the high halves (hi pointer) and low halves (lo pointer) of q0-q3,
 * four lanes each, as four per-lane runs of 4 x s16, lane l at -256 * l bytes;
 * \adv moves both pointers on afterwards. Needs q7 = 0x8000 x4. */
.macro SPLIT_STORE hi, lo, adv
    esp.vadd.s32 q4, q0, q7
    esp.vadd.s32 q5, q1, q7
    esp.vunzip.16 q4, q5
    esp.vadd.s32 q4, q2, q7
    esp.vadd.s32 q6, q3, q7
    esp.vunzip.16 q4, q6
    esp.vzip.16 q5, q6
    esp.vzip.16 q5, q6
    esp.vst.l.64.ip q5, \hi, -256
    esp.vst.h.64.ip q5, \hi, -256
    esp.vst.l.64.ip q6, \hi, -256
    esp.vst.h.64.ip q6, \hi, \adv
    esp.vunzip.16 q0, q1
    esp.vunzip.16 q2, q3
    esp.vzip.16 q0, q2
    esp.vzip.16 q0, q2
    esp.vst.l.64.ip q0, \lo, -256
    esp.vst.h.64.ip q0, \lo, -256
    esp.vst.l.64.ip q2, \lo, -256
    esp.vst.h.64.ip q2, \lo, \adv
.endm

/* void aac_k_syn_v4(int16_t *hi, int16_t *lo, const int32_t *a, const int32_t *b); see aac_k_syn_v4_c. */
    .balign 4
    .global aac_k_syn_v4
    .type aac_k_syn_v4, @function
aac_k_syn_v4:
    la t3, aac_k_half_lsb
    esp.vld.128.ip q7, t3, 0
    addi t3, a0, -256 + 124 * 2
    addi t4, a1, -256 + 124 * 2
    addi a0, a0, -256
    addi a1, a1, -256
    li a5, 16
1:
    mv a4, a2
    mv t5, a3
    esp.zero.q q6
    esp.vld.128.ip q4, a4, 16
    esp.vld.128.ip q5, t5, 16
    esp.vsub.s32 q0, q5, q4
    esp.vadd.s32 q0, q0, q0
    esp.vld.128.ip q4, a4, 16
    esp.vld.128.ip q5, t5, 16
    esp.vadd.s32 q1, q4, q5
    esp.vsub.s32 q1, q6, q1
    esp.vadd.s32 q1, q1, q1
    esp.vld.128.ip q4, a4, 16
    esp.vld.128.ip q5, t5, 16
    esp.vsub.s32 q2, q5, q4
    esp.vadd.s32 q2, q2, q2
    esp.vld.128.ip q4, a4, 16
    esp.vld.128.ip q5, t5, 16
    esp.vadd.s32 q3, q4, q5
    esp.vsub.s32 q3, q6, q3
    esp.vadd.s32 q3, q3, q3
    SPLIT_STORE a0, a1, 776

    addi a4, a2, 48
    addi t5, a3, 48
    esp.vld.128.ip q4, a4, -16
    esp.vld.128.ip q5, t5, -16
    esp.vsub.s32 q0, q4, q5
    esp.vadd.s32 q0, q0, q0
    esp.vld.128.ip q4, a4, -16
    esp.vld.128.ip q5, t5, -16
    esp.vadd.s32 q1, q4, q5
    esp.vadd.s32 q1, q1, q1
    esp.vld.128.ip q4, a4, -16
    esp.vld.128.ip q5, t5, -16
    esp.vsub.s32 q2, q4, q5
    esp.vadd.s32 q2, q2, q2
    esp.vld.128.ip q4, a4, -16
    esp.vld.128.ip q5, t5, -16
    esp.vadd.s32 q3, q4, q5
    esp.vadd.s32 q3, q3, q3
    SPLIT_STORE t3, t4, 760

    addi a2, a2, 64
    addi a3, a3, 64
    addi a5, a5, -1
    bnez a5, 1b
    fence
    ret
    .size aac_k_syn_v4, .-aac_k_syn_v4
