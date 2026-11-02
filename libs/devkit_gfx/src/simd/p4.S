/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-P4 PIE kernels of simd/mod.rs; its scalar loops write the same pixels.
 */

    .section .rodata
    .balign 16
/* In the order the loop reads them. */
gfx_p4_blend_consts:
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

/* q3 = q1 over q0 by q7 = a5 and q6 = 32 - a5 in each field, which is what
 * mix565 computes: (front * a5 + back * (32 - a5) + 16) / 32. SAR stays 0, so
 * the products are exact. A right shift by VSLD.16 brings the sign bit in, so
 * every field is masked. Reads the constants through t3, back where it was. */
.macro MIX565
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
.endm

/* CFG gets only the misaligned access bits. a5 and 32 - a5 go to q7 and q6
 * through the stack, which leaves t4 at 4(sp) for one more value. */
.macro SETUP a5
    addi sp, sp, -16
    esp.movx.r.cfg t6
    ori t6, t6, 3
    esp.movx.w.cfg t6
    li t6, 0
    esp.movx.w.sar t6
    li t6, 32
    sub t6, t6, \a5
    sh \a5, 0(sp)
    sh t6, 2(sp)
    mv t4, sp
    esp.vldbc.16.ip q7, t4, 2
    esp.vldbc.16.ip q6, t4, 2
    la t3, gfx_p4_blend_consts
.endm

    .text

/* void gfx_p4_blend565(uint16_t *out, const uint16_t *back,
 *                      const uint16_t *front, int n8, int a5)
 * n8 groups of 8 RGB565 pixels; out may be back. */
    .balign 4
    .global gfx_p4_blend565
    .type gfx_p4_blend565, @function
gfx_p4_blend565:
    SETUP a4
    blez a3, 2f
1:
    esp.vld.128.ip q0, a1, 16
    esp.vld.128.ip q1, a2, 16
    MIX565
    esp.vst.128.ip q3, a0, 16
    addi a3, a3, -1
    bnez a3, 1b
2:
    addi sp, sp, 16
    ret
    .size gfx_p4_blend565, .-gfx_p4_blend565

/* void gfx_p4_tint888(uint8_t *out, int n16, const uint8_t *pattern, int a)
 * pattern, 48 bytes of the color as the pixels hold it, over n16 groups of 16
 * B, G, R pixels in place by a of 256, 0 < a < 256:
 * (pixel * (256 - a) + color * a) >> 8 in each byte, which is what blend888
 * computes. Both products of a byte add in QACC and shift back by 8, rounding
 * down: vxrm is set to FLOOR, SAR to 8 and back to 0. */
    .balign 4
    .global gfx_p4_tint888
    .type gfx_p4_tint888, @function
gfx_p4_tint888:
    addi sp, sp, -16
    esp.movx.r.cfg t6
    andi t6, t6, -241
    ori t6, t6, 3
    esp.movx.w.cfg t6
    li t6, 8
    esp.movx.w.sar t6
    li t5, 256
    sub t5, t5, a3
    sb t5, 0(sp)
    sb a3, 1(sp)
    mv t4, sp
    esp.vldbc.8.ip q5, t4, 1                /* 256 - a */
    esp.vldbc.8.ip q6, t4, 0                /* a */
    esp.vld.128.ip q2, a2, 16
    esp.vld.128.ip q3, a2, 16
    esp.vld.128.ip q4, a2, 16
    mv a4, a0
    blez a1, 2f
1:
    esp.vld.128.ip q0, a4, 16
    esp.zero.qacc
    esp.vmulas.u8.qacc q0, q5
    esp.vmulas.u8.qacc q2, q6
    esp.srcmb.u8.qacc q1, t6, 0
    esp.vst.128.ip q1, a0, 16
    esp.vld.128.ip q0, a4, 16
    esp.zero.qacc
    esp.vmulas.u8.qacc q0, q5
    esp.vmulas.u8.qacc q3, q6
    esp.srcmb.u8.qacc q1, t6, 0
    esp.vst.128.ip q1, a0, 16
    esp.vld.128.ip q0, a4, 16
    esp.zero.qacc
    esp.vmulas.u8.qacc q0, q5
    esp.vmulas.u8.qacc q4, q6
    esp.srcmb.u8.qacc q1, t6, 0
    esp.vst.128.ip q1, a0, 16
    addi a1, a1, -1
    bnez a1, 1b
2:
    li t6, 0
    esp.movx.w.sar t6
    addi sp, sp, 16
    ret
    .size gfx_p4_tint888, .-gfx_p4_tint888

/* void gfx_p4_tint565(uint16_t *out, int n8, int color, int a5)
 * color over n8 groups of 8 RGB565 pixels, in place. */
    .balign 4
    .global gfx_p4_tint565
    .type gfx_p4_tint565, @function
gfx_p4_tint565:
    SETUP a3
    sh a2, 4(sp)
    esp.vldbc.16.ip q1, t4, 0
    mv a2, a0
    blez a1, 2f
1:
    esp.vld.128.ip q0, a2, 16
    MIX565
    esp.vst.128.ip q3, a0, 16
    addi a1, a1, -1
    bnez a1, 1b
2:
    addi sp, sp, 16
    ret
    .size gfx_p4_tint565, .-gfx_p4_tint565
