/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-P4 PIE kernel of zoom.rs; blend_line's scalar loop writes the same pixels.
 */

    .section .rodata
    .balign 16
/* In the order the loop reads them. */
zoom_blend_consts:
    .short 0x001f, 0x001f, 0x001f, 0x001f, 0x001f, 0x001f, 0x001f, 0x001f
    .short 16, 16, 16, 16, 16, 16, 16, 16
    .short -5, -5, -5, -5, -5, -5, -5, -5
    .short -5, -5, -5, -5, -5, -5, -5, -5
    .short 0x003f, 0x003f, 0x003f, 0x003f, 0x003f, 0x003f, 0x003f, 0x003f
    .short 16, 16, 16, 16, 16, 16, 16, 16
    .short 0x07e0, 0x07e0, 0x07e0, 0x07e0, 0x07e0, 0x07e0, 0x07e0, 0x07e0
    .short -11, -11, -11, -11, -11, -11, -11, -11
    .short 0x001f, 0x001f, 0x001f, 0x001f, 0x001f, 0x001f, 0x001f, 0x001f
    .short 16, 16, 16, 16, 16, 16, 16, 16
    .short 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0
    .short 6, 6, 6, 6, 6, 6, 6, 6

    .text

/* void zoom_k_blend565(uint16_t *out, const uint16_t *back, const uint16_t *front,
 *                      int n8, int a5)
 *
 * n8 groups of 8 RGB565 pixels: out = (front * a5 + back * (32 - a5) + 16) / 32
 * in each field, which is what the scalar blend computes. SAR stays 0, so the
 * products are exact; CFG gets only the misaligned access bits. A right
 * shift by VSLD.16 brings the sign bit in, so every field is masked. */
    .balign 4
    .global zoom_k_blend565
    .type zoom_k_blend565, @function
zoom_k_blend565:
    addi sp, sp, -16
    esp.movx.r.cfg t6
    ori t6, t6, 3
    esp.movx.w.cfg t6
    li t6, 0
    esp.movx.w.sar t6
    li t6, 32
    sub t6, t6, a4
    sh a4, 0(sp)
    sh t6, 2(sp)
    mv t4, sp
    esp.vldbc.16.ip q7, t4, 2               /* a5 */
    esp.vldbc.16.ip q6, t4, 0               /* 32 - a5 */
    la t3, zoom_blend_consts
    blez a3, 2f
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a2, 16
    /* blue */
    esp.vld.128.ip q2, t3, 16
    esp.andq q4, q0, q2
    esp.andq q5, q1, q2
    esp.vmul.u16 q4, q4, q6
    esp.vmul.u16 q5, q5, q7
    esp.vadd.s16 q4, q4, q5
    esp.vld.128.ip q2, t3, 16
    esp.vadd.s16 q4, q4, q2
    esp.vld.128.ip q2, t3, 16
    esp.vsld.16 q3, q4, q2
    /* green */
    esp.vld.128.ip q2, t3, 16
    esp.vsld.16 q4, q0, q2
    esp.vsld.16 q5, q1, q2
    esp.vld.128.ip q2, t3, 16
    esp.andq q4, q4, q2
    esp.andq q5, q5, q2
    esp.vmul.u16 q4, q4, q6
    esp.vmul.u16 q5, q5, q7
    esp.vadd.s16 q4, q4, q5
    esp.vld.128.ip q2, t3, 16
    esp.vadd.s16 q4, q4, q2
    esp.vld.128.ip q2, t3, 16
    esp.andq q4, q4, q2
    esp.orq q3, q3, q4
    /* red */
    esp.vld.128.ip q2, t3, 16
    esp.vsld.16 q4, q0, q2
    esp.vsld.16 q5, q1, q2
    esp.vld.128.ip q2, t3, 16
    esp.andq q4, q4, q2
    esp.andq q5, q5, q2
    esp.vmul.u16 q4, q4, q6
    esp.vmul.u16 q5, q5, q7
    esp.vadd.s16 q4, q4, q5
    esp.vld.128.ip q2, t3, 16
    esp.vadd.s16 q4, q4, q2
    esp.vld.128.ip q2, t3, 16
    esp.andq q4, q4, q2
    esp.vld.128.ip q2, t3, -176
    esp.vsld.16 q4, q4, q2
    esp.orq q3, q3, q4
    esp.vst.128.ip q3, a0, 16
    addi a3, a3, -1
    bnez a3, 1b
2:
    addi sp, sp, 16
    ret
    .size zoom_k_blend565, .-zoom_k_blend565
