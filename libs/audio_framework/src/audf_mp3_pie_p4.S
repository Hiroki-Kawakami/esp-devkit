/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

    .text

/* CFG is global and not saved per task, so every decode call sets the
 * misaligned load/store bits itself. SAR must be left at 0. */
    .balign 4
    .global mp3_k_prepare
    .type mp3_k_prepare, @function
mp3_k_prepare:
    esp.movx.r.cfg t6
    ori t6, t6, 3
    esp.movx.w.cfg t6
    li t6, 0
    esp.movx.w.sar t6
    ret
    .size mp3_k_prepare, .-mp3_k_prepare

/* t5 + t6 to out, out += 4 */
.macro DOT_STORE
    esp.srs.s.xacc t6, t3
    add t5, t5, t6
    sw t5, 0(a0)
    addi a0, a0, 4
.endm

/* void mp3_k_dot16(int32_t *out, const int16_t *xh, const int16_t *xl, const int16_t *c, int rows) */
    .balign 4
    .global mp3_k_dot16
    .type mp3_k_dot16, @function
mp3_k_dot16:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    esp.vld.128.ip q2, a2, 16
    esp.vld.128.ip q3, a2, 16
    li t3, 16
    li t4, 0
1:
    esp.zero.xacc
    esp.vld.128.ip q6, a3, 16
    esp.vmulas.s16.xacc.ld.ip q7, a3, 16, q0, q6
    esp.vmulas.s16.xacc.ld.ip q6, a3, 16, q1, q7
    esp.srs.s.xacc t5, t4
    esp.zero.xacc
    esp.vmulas.s16.xacc.ld.ip q7, a3, -48, q0, q6
    esp.vmulas.s16.xacc.ld.ip q6, a3, 16, q1, q7
    esp.vmulas.s16.xacc.ld.ip q7, a3, 48, q2, q6
    esp.vmulas.s16.xacc q3, q7
    DOT_STORE
    addi a4, a4, -1
    bnez a4, 1b
    ret
    .size mp3_k_dot16, .-mp3_k_dot16

/* void mp3_k_dot24(int32_t *out, const int16_t *xh, const int16_t *xl, const int16_t *c, int rows) */
    .balign 4
    .global mp3_k_dot24
    .type mp3_k_dot24, @function
mp3_k_dot24:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a1, 16
    esp.vld.128.ip q2, a1, 16
    esp.vld.128.ip q3, a2, 16
    esp.vld.128.ip q4, a2, 16
    esp.vld.128.ip q5, a2, 16
    li t3, 16
    li t4, 0
1:
    esp.zero.xacc
    esp.vld.128.ip q6, a3, 16
    esp.vmulas.s16.xacc.ld.ip q7, a3, 16, q0, q6
    esp.vmulas.s16.xacc.ld.ip q6, a3, 16, q1, q7
    esp.vmulas.s16.xacc.ld.ip q7, a3, 16, q2, q6
    esp.srs.s.xacc t5, t4
    esp.zero.xacc
    esp.vmulas.s16.xacc.ld.ip q6, a3, 16, q0, q7
    esp.vmulas.s16.xacc.ld.ip q7, a3, -80, q1, q6
    esp.vmulas.s16.xacc.ld.ip q6, a3, 16, q2, q7
    esp.vmulas.s16.xacc.ld.ip q7, a3, 16, q3, q6
    esp.vmulas.s16.xacc.ld.ip q6, a3, 64, q4, q7
    esp.vmulas.s16.xacc q5, q6
    DOT_STORE
    addi a4, a4, -1
    bnez a4, 1b
    ret
    .size mp3_k_dot24, .-mp3_k_dot24

/* q6/q7 += the 32-bit products of 8 x s16 at t3 + off and at a3; a3 += 16 */
.macro WROW off
    addi t5, t3, \off
    esp.vld.128.ip q0, t5, 0
    esp.vld.128.ip q1, a3, 16
    esp.vmul.s32.s16xs16 q2, q3, q0, q1
    esp.vadd.s32 q6, q6, q2
    esp.vadd.s32 q7, q7, q3
.endm

/* The rows of mp3_window_split in order, at mp3_window_off * 2 bytes. */
.macro WPASS
    WROW 0
    WROW 192
    WROW 256
    WROW 448
    WROW 512
    WROW 704
    WROW 768
    WROW 960
    WROW 1024
    WROW 1216
    WROW 1280
    WROW 1472
    WROW 1536
    WROW 1728
    WROW 1792
    WROW 1984
    WROW 768
    WROW 960
    WROW 1024
    WROW 1216
    WROW 960
    WROW 1024
.endm

/* Every kernel that stores with esp.vst ends with a fence: without it a scalar
 * load right after the call can still see the old contents. */

/* void mp3_k_window(int32_t *acc, const int16_t *vh, const int16_t *vl, const int16_t *c) */
    .balign 4
    .global mp3_k_window
    .type mp3_k_window, @function
mp3_k_window:
    li t4, 4
    li t6, 16
1:
    esp.zero.q q6
    esp.zero.q q7
    li t5, 0
    esp.movx.w.sar t5
    mv t3, a1
    WPASS
    addi a3, a3, -352
    esp.movx.w.sar t6
    mv t3, a2
    WPASS
    esp.vst.128.ip q6, a0, 16
    esp.vst.128.ip q7, a0, 16
    addi a1, a1, 16
    addi a2, a2, 16
    addi t4, t4, -1
    bnez t4, 1b
    li t5, 0
    esp.movx.w.sar t5
    fence
    ret
    .size mp3_k_window, .-mp3_k_window
