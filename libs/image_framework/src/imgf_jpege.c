/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Baseline JPEG encoder. 4:4:4 / 4:2:2 / 4:2:0 chroma subsampling for color
 * inputs; grayscale always single-component. Hot path is integer-only:
 *   - YCbCr conversion: integer fixed-point (BT.601, ×256).
 *   - FDCT: integer AAN (Q12 constants), no float multiplies per sample.
 *   - Quantize: integer sign-aware round-divide by a per-coefficient table
 *     premultiplied with AAN factors at create time.
 * Standard Annex K Huffman tables for both luma and chroma. Quality 1..100.
 */

#include "imgf_jpege.h"

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_encoder.h"

/* ---- static tables ---------------------------------------------------- */

static const uint8_t kZigzag[64] = {
     0,  1,  8, 16,  9,  2,  3, 10, 17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63};

static const uint8_t kStdQ_L[64] = {
    16, 11, 10, 16, 24, 40, 51, 61,
    12, 12, 14, 19, 26, 58, 60, 55,
    14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68,109,103, 77,
    24, 35, 55, 64, 81,104,113, 92,
    49, 64, 78, 87,103,121,120,101,
    72, 92, 95, 98,112,100,103, 99,
};
static const uint8_t kStdQ_C[64] = {
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
};

static const uint8_t kDC_L_bits[17] = {0, 0,1,5,1,1,1,1,1,1,0,0,0,0,0,0,0};
static const uint8_t kDC_L_vals[12] = {0,1,2,3,4,5,6,7,8,9,10,11};
static const uint8_t kDC_C_bits[17] = {0, 0,3,1,1,1,1,1,1,1,1,1,0,0,0,0,0};
static const uint8_t kDC_C_vals[12] = {0,1,2,3,4,5,6,7,8,9,10,11};
static const uint8_t kAC_L_bits[17] = {0, 0,2,1,3,3,2,4,3,5,5,4,4,0,0,1,0x7d};
static const uint8_t kAC_L_vals[162] = {
    0x01,0x02,0x03,0x00,0x04,0x11,0x05,0x12,0x21,0x31,0x41,0x06,0x13,0x51,0x61,0x07,
    0x22,0x71,0x14,0x32,0x81,0x91,0xa1,0x08,0x23,0x42,0xb1,0xc1,0x15,0x52,0xd1,0xf0,
    0x24,0x33,0x62,0x72,0x82,0x09,0x0a,0x16,0x17,0x18,0x19,0x1a,0x25,0x26,0x27,0x28,
    0x29,0x2a,0x34,0x35,0x36,0x37,0x38,0x39,0x3a,0x43,0x44,0x45,0x46,0x47,0x48,0x49,
    0x4a,0x53,0x54,0x55,0x56,0x57,0x58,0x59,0x5a,0x63,0x64,0x65,0x66,0x67,0x68,0x69,
    0x6a,0x73,0x74,0x75,0x76,0x77,0x78,0x79,0x7a,0x83,0x84,0x85,0x86,0x87,0x88,0x89,
    0x8a,0x92,0x93,0x94,0x95,0x96,0x97,0x98,0x99,0x9a,0xa2,0xa3,0xa4,0xa5,0xa6,0xa7,
    0xa8,0xa9,0xaa,0xb2,0xb3,0xb4,0xb5,0xb6,0xb7,0xb8,0xb9,0xba,0xc2,0xc3,0xc4,0xc5,
    0xc6,0xc7,0xc8,0xc9,0xca,0xd2,0xd3,0xd4,0xd5,0xd6,0xd7,0xd8,0xd9,0xda,0xe1,0xe2,
    0xe3,0xe4,0xe5,0xe6,0xe7,0xe8,0xe9,0xea,0xf1,0xf2,0xf3,0xf4,0xf5,0xf6,0xf7,0xf8,
    0xf9,0xfa,
};
static const uint8_t kAC_C_bits[17] = {0, 0,2,1,2,4,4,3,4,7,5,4,4,0,1,2,0x77};
static const uint8_t kAC_C_vals[162] = {
    0x00,0x01,0x02,0x03,0x11,0x04,0x05,0x21,0x31,0x06,0x12,0x41,0x51,0x07,0x61,0x71,
    0x13,0x22,0x32,0x81,0x08,0x14,0x42,0x91,0xa1,0xb1,0xc1,0x09,0x23,0x33,0x52,0xf0,
    0x15,0x62,0x72,0xd1,0x0a,0x16,0x24,0x34,0xe1,0x25,0xf1,0x17,0x18,0x19,0x1a,0x26,
    0x27,0x28,0x29,0x2a,0x35,0x36,0x37,0x38,0x39,0x3a,0x43,0x44,0x45,0x46,0x47,0x48,
    0x49,0x4a,0x53,0x54,0x55,0x56,0x57,0x58,0x59,0x5a,0x63,0x64,0x65,0x66,0x67,0x68,
    0x69,0x6a,0x73,0x74,0x75,0x76,0x77,0x78,0x79,0x7a,0x82,0x83,0x84,0x85,0x86,0x87,
    0x88,0x89,0x8a,0x92,0x93,0x94,0x95,0x96,0x97,0x98,0x99,0x9a,0xa2,0xa3,0xa4,0xa5,
    0xa6,0xa7,0xa8,0xa9,0xaa,0xb2,0xb3,0xb4,0xb5,0xb6,0xb7,0xb8,0xb9,0xba,0xc2,0xc3,
    0xc4,0xc5,0xc6,0xc7,0xc8,0xc9,0xca,0xd2,0xd3,0xd4,0xd5,0xd6,0xd7,0xd8,0xd9,0xda,
    0xe2,0xe3,0xe4,0xe5,0xe6,0xe7,0xe8,0xe9,0xea,0xf2,0xf3,0xf4,0xf5,0xf6,0xf7,0xf8,
    0xf9,0xfa,
};

/* ---- AAN constants (Q12 fixed-point) --------------------------------- */

#define K_0_382  1567   /* 0.382683433 * 4096 */
#define K_0_541  2217   /* 0.541196100 * 4096 */
#define K_0_707  2896   /* 0.707106781 * 4096 */
#define K_1_306  5352   /* 1.306562965 * 4096 */

/* AAN per-frequency scale factors, exact: 0.5 / cos(k*pi/16). aan_scale[0] is
 * 1/sqrt(8) but we follow libjpeg's convention and use 1.0 there (the missing
 * factor of sqrt(8) cancels with another factor below). The encoder bakes
 * (aan_scale[u] * aan_scale[v] * 8) into the quantizer at create time so the
 * AAN FDCT output divides cleanly. */
static const double kAanScale[8] = {
    1.0, 1.387039845322148, 1.306562964876377, 1.175875602419359,
    1.0, 0.785694958387102, 0.541196100146197, 0.275899379282943,
};

/* ---- state ------------------------------------------------------------ */

struct imgf_jpege {
    int width, height;
    int n_comp;                 /* 1 (gray) or 3 (YCbCr) */
    imgf_pixfmt_t input_pf;

    int sub_h, sub_v;           /* luma sampling: (1,1) / (2,1) / (2,2) */
    int mcu_w, mcu_h;           /* 8*sub_h, 8*sub_v */
    int width_pad, height_pad;  /* rounded up to MCU boundaries */

    /* quantization */
    uint8_t qt_byte[2][64];     /* natural order, for DQT marker */
    int     qt_aan [2][64];     /* natural order, * 8 * aan_scale[u]*aan_scale[v] */

    /* Huffman encode tables: [DC/AC][table id][symbol] -> (code, length) */
    uint16_t hdc_code[2][16];
    uint8_t  hdc_len [2][16];
    uint16_t hac_code[2][256];
    uint8_t  hac_len [2][256];

    /* MCU row buffer: mcu_h rows of width_pad samples per channel (YCbCr level
     * shifted). Allocated once. */
    int16_t *block_buf;
    int      rows_in_buf;
    int      total_rows;
    int      mcu_rows_done;

    int dc_pred[3];

    uint32_t bitbuf;
    int      bitcnt;

    uint8_t *dst;
    size_t   dst_cap;
    size_t   dst_pos;
    uint8_t  overflow;
    uint8_t  bound;
};

/* ---- helpers: Huffman code builder ----------------------------------- */

static void build_huff_codes(const uint8_t *bits, const uint8_t *vals,
                             uint16_t *codes, uint8_t *lens, int max_syms) {
    memset(codes, 0, sizeof(uint16_t) * max_syms);
    memset(lens,  0, sizeof(uint8_t)  * max_syms);
    int code = 0, k = 0;
    for (int len = 1; len <= 16; len++) {
        for (int i = 0; i < bits[len]; i++) {
            int sym = vals[k++];
            codes[sym] = (uint16_t)code;
            lens [sym] = (uint8_t)len;
            code++;
        }
        code <<= 1;
    }
}

static void scale_qtab(const uint8_t *std, uint8_t *out, int quality) {
    int scale = quality < 50 ? 5000 / quality : 200 - 2 * quality;
    for (int i = 0; i < 64; i++) {
        int v = (std[i] * scale + 50) / 100;
        if (v < 1)   v = 1;
        if (v > 255) v = 255;
        out[i] = (uint8_t)v;
    }
}

/* ---- bit writer ------------------------------------------------------- */

static void put_byte(imgf_jpege_t *j, uint8_t b) {
    if (j->dst_pos >= j->dst_cap) { j->overflow = 1; return; }
    j->dst[j->dst_pos++] = b;
}

static void put_word(imgf_jpege_t *j, uint16_t w) {
    put_byte(j, (uint8_t)(w >> 8));
    put_byte(j, (uint8_t)(w & 0xFF));
}

static void put_marker(imgf_jpege_t *j, uint8_t m) {
    put_byte(j, 0xFF);
    put_byte(j, m);
}

static void put_bits(imgf_jpege_t *j, uint32_t value, int nbits) {
    j->bitbuf = (j->bitbuf << nbits) | (value & ((1u << nbits) - 1));
    j->bitcnt += nbits;
    while (j->bitcnt >= 8) {
        j->bitcnt -= 8;
        uint8_t b = (uint8_t)((j->bitbuf >> j->bitcnt) & 0xFF);
        put_byte(j, b);
        if (b == 0xFF) put_byte(j, 0x00);   /* byte stuffing */
    }
}

static void flush_bits(imgf_jpege_t *j) {
    if (j->bitcnt > 0) {
        int pad = 8 - j->bitcnt;
        put_bits(j, (1u << pad) - 1, pad);
    }
}

/* ---- header writers --------------------------------------------------- */

static void write_dqt(imgf_jpege_t *j, int table_id, const uint8_t *table) {
    put_marker(j, 0xDB);
    put_word(j, 3 + 64);
    put_byte(j, (uint8_t)table_id);
    for (int i = 0; i < 64; i++) put_byte(j, table[kZigzag[i]]);
}

static void write_dht(imgf_jpege_t *j, int tc, int th, const uint8_t *bits, const uint8_t *vals) {
    int total = 0;
    for (int i = 1; i <= 16; i++) total += bits[i];
    put_marker(j, 0xC4);
    put_word(j, (uint16_t)(3 + 16 + total));
    put_byte(j, (uint8_t)((tc << 4) | th));
    for (int i = 1; i <= 16; i++) put_byte(j, bits[i]);
    for (int i = 0; i < total; i++) put_byte(j, vals[i]);
}

static void write_sof0(imgf_jpege_t *j) {
    put_marker(j, 0xC0);
    put_word(j, (uint16_t)(8 + 3 * j->n_comp));
    put_byte(j, 8);
    put_word(j, (uint16_t)j->height);
    put_word(j, (uint16_t)j->width);
    put_byte(j, (uint8_t)j->n_comp);
    for (int c = 0; c < j->n_comp; c++) {
        put_byte(j, (uint8_t)(c + 1));
        if (c == 0)
            put_byte(j, (uint8_t)((j->sub_h << 4) | j->sub_v));   /* Y sampling */
        else
            put_byte(j, 0x11);                                    /* chroma 1:1 */
        put_byte(j, (uint8_t)(c == 0 ? 0 : 1));
    }
}

static void write_sos(imgf_jpege_t *j) {
    put_marker(j, 0xDA);
    put_word(j, (uint16_t)(6 + 2 * j->n_comp));
    put_byte(j, (uint8_t)j->n_comp);
    for (int c = 0; c < j->n_comp; c++) {
        put_byte(j, (uint8_t)(c + 1));
        put_byte(j, (uint8_t)(c == 0 ? 0x00 : 0x11));
    }
    put_byte(j, 0);
    put_byte(j, 63);
    put_byte(j, 0);
}

static imgf_err_t write_headers(imgf_jpege_t *j) {
    put_marker(j, 0xD8);   /* SOI */
    write_dqt(j, 0, j->qt_byte[0]);
    if (j->n_comp == 3) write_dqt(j, 1, j->qt_byte[1]);
    write_sof0(j);
    write_dht(j, 0, 0, kDC_L_bits, kDC_L_vals);
    write_dht(j, 1, 0, kAC_L_bits, kAC_L_vals);
    if (j->n_comp == 3) {
        write_dht(j, 0, 1, kDC_C_bits, kDC_C_vals);
        write_dht(j, 1, 1, kAC_C_bits, kAC_C_vals);
    }
    write_sos(j);
    return j->overflow ? IMGF_ERR_OOM : IMGF_OK;
}

/* ---- AAN integer FDCT ------------------------------------------------- */

/* Q12 multiply: (v * c) >> 12, with c already pre-scaled by 4096. */
#define AAN_MULT(v, c) (((v) * (c)) >> 12)

static void fdct_1d(int *d) {
    int tmp0 = d[0] + d[7], tmp7 = d[0] - d[7];
    int tmp1 = d[1] + d[6], tmp6 = d[1] - d[6];
    int tmp2 = d[2] + d[5], tmp5 = d[2] - d[5];
    int tmp3 = d[3] + d[4], tmp4 = d[3] - d[4];

    /* Even part */
    int tmp10 = tmp0 + tmp3, tmp13 = tmp0 - tmp3;
    int tmp11 = tmp1 + tmp2, tmp12 = tmp1 - tmp2;

    d[0] = tmp10 + tmp11;
    d[4] = tmp10 - tmp11;
    int z1 = AAN_MULT(tmp12 + tmp13, K_0_707);
    d[2] = tmp13 + z1;
    d[6] = tmp13 - z1;

    /* Odd part */
    tmp10 = tmp4 + tmp5;
    tmp11 = tmp5 + tmp6;
    tmp12 = tmp6 + tmp7;

    int z5 = AAN_MULT(tmp10 - tmp12, K_0_382);
    int z2 = AAN_MULT(tmp10, K_0_541) + z5;
    int z4 = AAN_MULT(tmp12, K_1_306) + z5;
    int z3 = AAN_MULT(tmp11, K_0_707);

    int z11 = tmp7 + z3;
    int z13 = tmp7 - z3;

    d[5] = z13 + z2;
    d[3] = z13 - z2;
    d[1] = z11 + z4;
    d[7] = z11 - z4;
}

static void fdct_8x8(int *block) {
    for (int y = 0; y < 8; y++) fdct_1d(block + y * 8);
    int col[8];
    for (int x = 0; x < 8; x++) {
        for (int i = 0; i < 8; i++) col[i] = block[i * 8 + x];
        fdct_1d(col);
        for (int i = 0; i < 8; i++) block[i * 8 + x] = col[i];
    }
}

/* ---- block encode ----------------------------------------------------- */

static int magnitude(int v) {
    int a = v < 0 ? -v : v;
    int n = 0;
    while (a) { n++; a >>= 1; }
    return n;
}

static int extend_encode(int v, int nbits) {
    if (v < 0) v -= 1;
    return v & ((1 << nbits) - 1);
}

/* Sign-aware rounding integer divide: round(v / q) toward nearest. */
static inline int sdiv(int v, int q) {
    return v >= 0 ? (v + q / 2) / q : -(((-v) + q / 2) / q);
}

static void encode_block(imgf_jpege_t *j, int *block, int comp) {
    int qid = comp == 0 ? 0 : 1;
    int hid = comp == 0 ? 0 : 1;
    const int *qaan = j->qt_aan[qid];

    fdct_8x8(block);

    int zz[64];
    for (int k = 0; k < 64; k++) {
        int nat = kZigzag[k];
        zz[k] = sdiv(block[nat], qaan[nat]);
    }

    /* DC */
    int diff = zz[0] - j->dc_pred[comp];
    j->dc_pred[comp] = zz[0];
    int n = magnitude(diff);
    put_bits(j, j->hdc_code[hid][n], j->hdc_len[hid][n]);
    if (n) put_bits(j, (uint32_t)extend_encode(diff, n), n);

    /* AC */
    int last_nz = 63;
    while (last_nz > 0 && zz[last_nz] == 0) last_nz--;
    int run = 0;
    for (int k = 1; k <= last_nz; k++) {
        if (zz[k] == 0) {
            run++;
        } else {
            while (run >= 16) {
                put_bits(j, j->hac_code[hid][0xF0], j->hac_len[hid][0xF0]);
                run -= 16;
            }
            int nb = magnitude(zz[k]);
            int sym = (run << 4) | nb;
            put_bits(j, j->hac_code[hid][sym], j->hac_len[hid][sym]);
            put_bits(j, (uint32_t)extend_encode(zz[k], nb), nb);
            run = 0;
        }
    }
    if (last_nz < 63) {
        put_bits(j, j->hac_code[hid][0x00], j->hac_len[hid][0x00]);
    }
}

/* ---- MCU row encode --------------------------------------------------- */

/* Extract a Y/luma 8x8 block at (x_in_mcu, y_in_mcu) within the current MCU
 * at horizontal offset x0 (in pixels). */
static void extract_y_block(imgf_jpege_t *j, int x0, int by, int bx, int *out) {
    int stride = j->width_pad * j->n_comp;
    for (int y = 0; y < 8; y++) {
        const int16_t *row = j->block_buf
            + (size_t)(by * 8 + y) * stride
            + (size_t)(x0 + bx * 8) * j->n_comp;
        if (j->n_comp == 1) {
            for (int x = 0; x < 8; x++) out[y * 8 + x] = row[x];
        } else {
            for (int x = 0; x < 8; x++) out[y * 8 + x] = row[x * 3 + 0];  /* Y is comp 0 */
        }
    }
}

/* Extract a chroma 8x8 block by downsampling sub_h * sub_v cells per output. */
static void extract_chroma_block(imgf_jpege_t *j, int x0, int comp, int *out) {
    int stride = j->width_pad * 3;
    int area = j->sub_h * j->sub_v;
    int half = area / 2;
    for (int y = 0; y < 8; y++) {
        for (int x = 0; x < 8; x++) {
            int sum = 0;
            for (int vy = 0; vy < j->sub_v; vy++) {
                const int16_t *row = j->block_buf
                    + (size_t)(y * j->sub_v + vy) * stride
                    + (size_t)(x0 + x * j->sub_h) * 3 + comp;
                for (int hx = 0; hx < j->sub_h; hx++)
                    sum += row[hx * 3];
            }
            out[y * 8 + x] = (sum >= 0 ? sum + half : sum - half) / area;
        }
    }
}

static void encode_mcu_row(imgf_jpege_t *j) {
    int mcus = j->width_pad / j->mcu_w;
    for (int mx = 0; mx < mcus; mx++) {
        int x0 = mx * j->mcu_w;
        /* Y blocks in raster order within the MCU. */
        for (int by = 0; by < j->sub_v; by++) {
            for (int bx = 0; bx < j->sub_h; bx++) {
                int block[64];
                extract_y_block(j, x0, by, bx, block);
                encode_block(j, block, 0);
            }
        }
        if (j->n_comp == 3) {
            int blockCb[64], blockCr[64];
            extract_chroma_block(j, x0, 1, blockCb);
            encode_block(j, blockCb, 1);
            extract_chroma_block(j, x0, 2, blockCr);
            encode_block(j, blockCr, 2);
        }
    }
}

/* Convert one input row to YCbCr (or Y for gray), level-shifted, stored into
 * the MCU buffer at row j->rows_in_buf. Horizontal padding replicates the
 * rightmost pixel. */
static void absorb_row(imgf_jpege_t *j, const uint8_t *src) {
    int16_t *dst = j->block_buf
        + (size_t)j->rows_in_buf * j->width_pad * j->n_comp;
    int w = j->width;
    if (j->n_comp == 1) {
        for (int x = 0; x < w; x++) dst[x] = (int16_t)((int)src[x] - 128);
        for (int x = w; x < j->width_pad; x++) dst[x] = dst[w - 1];
    } else {
        for (int x = 0; x < w; x++) {
            int r = src[3 * x + 0];
            int g = src[3 * x + 1];
            int b = src[3 * x + 2];
            int yv  = ( 77 * r + 150 * g +  29 * b + 128) >> 8;
            int cbv = (-43 * r -  85 * g + 128 * b + 32896) >> 8;
            int crv = (128 * r - 107 * g -  21 * b + 32896) >> 8;
            dst[3 * x + 0] = (int16_t)(yv  - 128);
            dst[3 * x + 1] = (int16_t)(cbv - 128);
            dst[3 * x + 2] = (int16_t)(crv - 128);
        }
        for (int x = w; x < j->width_pad; x++) {
            dst[3 * x + 0] = dst[3 * (w - 1) + 0];
            dst[3 * x + 1] = dst[3 * (w - 1) + 1];
            dst[3 * x + 2] = dst[3 * (w - 1) + 2];
        }
    }
    j->rows_in_buf++;
}

static void pad_last_rows(imgf_jpege_t *j) {
    if (j->rows_in_buf == 0 || j->rows_in_buf >= j->mcu_h) return;
    int16_t *last = j->block_buf
        + (size_t)(j->rows_in_buf - 1) * j->width_pad * j->n_comp;
    size_t row_bytes = (size_t)j->width_pad * j->n_comp * sizeof(int16_t);
    while (j->rows_in_buf < j->mcu_h) {
        int16_t *dst = j->block_buf
            + (size_t)j->rows_in_buf * j->width_pad * j->n_comp;
        memcpy(dst, last, row_bytes);
        j->rows_in_buf++;
    }
}

/* ---- public ----------------------------------------------------------- */

size_t imgf_jpege_buffer_upper_bound(uint16_t w, uint16_t h, imgf_pixfmt_t pf) {
    int n = (pf == IMGF_PIX_GRAY8) ? 1 : 3;
    return (size_t)w * h * n * 2 + 1024;
}

static void resolve_subsample(int n_comp, int sub_opt, int *sh, int *sv) {
    if (n_comp == 1) { *sh = 1; *sv = 1; return; }
    switch (sub_opt) {
        case IMGF_JPEG_SUBSAMPLE_444: *sh = 1; *sv = 1; break;
        case IMGF_JPEG_SUBSAMPLE_422: *sh = 2; *sv = 1; break;
        case IMGF_JPEG_SUBSAMPLE_420: *sh = 2; *sv = 2; break;
        default:                       *sh = 2; *sv = 2; break;   /* default = 4:2:0 */
    }
}

imgf_jpege_t *imgf_jpege_create(uint16_t width, uint16_t height,
                                imgf_pixfmt_t input_pf,
                                int quality, int subsample,
                                uint32_t alloc_caps, imgf_err_t *out_err) {
    if (width == 0 || height == 0) {
        if (out_err) *out_err = IMGF_ERR_INVALID_ARG;
        return NULL;
    }
    if (input_pf != IMGF_PIX_GRAY8 && input_pf != IMGF_PIX_RGB888) {
        if (out_err) *out_err = IMGF_ERR_UNSUPPORTED;
        return NULL;
    }
    if (quality <= 0) quality = 75;
    if (quality > 100) quality = 100;
    if (quality < 1)   quality = 1;

    imgf_jpege_t *j = (imgf_jpege_t *)calloc(1, sizeof *j);
    if (!j) { if (out_err) *out_err = IMGF_ERR_OOM; return NULL; }
    j->width    = width;
    j->height   = height;
    j->input_pf = input_pf;
    j->n_comp   = (input_pf == IMGF_PIX_GRAY8) ? 1 : 3;
    resolve_subsample(j->n_comp, subsample, &j->sub_h, &j->sub_v);
    j->mcu_w = 8 * j->sub_h;
    j->mcu_h = 8 * j->sub_v;
    j->width_pad  = (width  + j->mcu_w - 1) & ~(j->mcu_w - 1);
    j->height_pad = (height + j->mcu_h - 1) & ~(j->mcu_h - 1);

    scale_qtab(kStdQ_L, j->qt_byte[0], quality);
    scale_qtab(kStdQ_C, j->qt_byte[1], quality);
    /* Premultiply: qaan[i] = std_q[i] * aan_scale[row] * aan_scale[col] * 8.
     * The factor of 8 absorbs the AAN FDCT's inherent scaling. */
    for (int t = 0; t < 2; t++) {
        for (int i = 0; i < 64; i++) {
            int row = i >> 3, col = i & 7;
            double q = j->qt_byte[t][i] * kAanScale[row] * kAanScale[col] * 8.0;
            int qi = (int)lround(q);
            if (qi < 1) qi = 1;
            j->qt_aan[t][i] = qi;
        }
    }

    build_huff_codes(kDC_L_bits, kDC_L_vals, j->hdc_code[0], j->hdc_len[0], 16);
    build_huff_codes(kDC_C_bits, kDC_C_vals, j->hdc_code[1], j->hdc_len[1], 16);
    build_huff_codes(kAC_L_bits, kAC_L_vals, j->hac_code[0], j->hac_len[0], 256);
    build_huff_codes(kAC_C_bits, kAC_C_vals, j->hac_code[1], j->hac_len[1], 256);

    size_t buf_bytes = (size_t)j->mcu_h * j->width_pad * j->n_comp * sizeof(int16_t);
    j->block_buf = (int16_t *)imgf_alloc(buf_bytes, alloc_caps);
    if (!j->block_buf) {
        free(j);
        if (out_err) *out_err = IMGF_ERR_OOM;
        return NULL;
    }
    memset(j->block_buf, 0, buf_bytes);

    if (out_err) *out_err = IMGF_OK;
    return j;
}

void imgf_jpege_destroy(imgf_jpege_t *j) {
    if (!j) return;
    if (j->block_buf) imgf_free(j->block_buf);
    free(j);
}

imgf_err_t imgf_jpege_bind(imgf_jpege_t *j, uint8_t *dst, size_t cap) {
    if (!j) return IMGF_ERR_INVALID_ARG;
    j->dst = dst;
    j->dst_cap = cap;
    j->dst_pos = 0;
    j->overflow = 0;
    j->bound = 1;
    j->total_rows = 0;
    j->rows_in_buf = 0;
    j->mcu_rows_done = 0;
    j->bitbuf = 0;
    j->bitcnt = 0;
    for (int i = 0; i < 3; i++) j->dc_pred[i] = 0;
    return write_headers(j);
}

imgf_err_t imgf_jpege_push(imgf_jpege_t *j, const uint8_t *row) {
    if (!j || !j->bound) return IMGF_ERR_INVALID_STATE;
    if (j->total_rows >= j->height) return IMGF_ERR_INVALID_STATE;
    absorb_row(j, row);
    j->total_rows++;
    if (j->rows_in_buf == j->mcu_h) {
        encode_mcu_row(j);
        j->rows_in_buf = 0;
        j->mcu_rows_done++;
    }
    return j->overflow ? IMGF_ERR_OOM : IMGF_OK;
}

imgf_err_t imgf_jpege_finish(imgf_jpege_t *j, size_t *bytes_written) {
    if (!j || !j->bound) return IMGF_ERR_INVALID_STATE;
    if (j->total_rows != j->height) return IMGF_ERR_INVALID_STATE;
    if (j->rows_in_buf > 0) {
        pad_last_rows(j);
        encode_mcu_row(j);
        j->rows_in_buf = 0;
        j->mcu_rows_done++;
    }
    flush_bits(j);
    put_marker(j, 0xD9);   /* EOI */
    if (bytes_written) *bytes_written = j->dst_pos;
    return j->overflow ? IMGF_ERR_OOM : IMGF_OK;
}
