/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * VP8 key frame decoder (RFC 6386) for WebP. Output matches libwebp: its
 * fancy chroma upsampling and its YUV to RGB arithmetic.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_webpd_internal.h"

#define BPS 32
#define Y_OFF (BPS + 8)
#define U_OFF (Y_OFF + BPS * 17)
#define V_OFF (U_OFF + 16)
#define WORK_SIZE (BPS * 26)

#define PART0_WINDOW 8192
#define TOKEN_WINDOW 32768

/* Rows kept above the macroblock row being decoded: the loop filter reads four
 * of them, and output trails the decoder by up to seven luma rows. */
#define CACHE_ABOVE 8

enum {
    B_DC_PRED, B_TM_PRED, B_VE_PRED, B_HE_PRED, B_RD_PRED,
    B_VR_PRED, B_LD_PRED, B_VL_PRED, B_HD_PRED, B_HU_PRED,
};
enum { DC_PRED = 0, TM_PRED = 1, V_PRED = 2, H_PRED = 3 };
enum { DC_NOTOP = 4, DC_NOLEFT = 5, DC_NOTOPLEFT = 6 };

static const uint8_t kCoeffProbs[4 * 8 * 3 * 11] = {
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    253, 136, 254, 255, 228, 219, 128, 128, 128, 128, 128,
    189, 129, 242, 255, 227, 213, 255, 219, 128, 128, 128,
    106, 126, 227, 252, 214, 209, 255, 255, 128, 128, 128,
      1,  98, 248, 255, 236, 226, 255, 255, 128, 128, 128,
    181, 133, 238, 254, 221, 234, 255, 154, 128, 128, 128,
     78, 134, 202, 247, 198, 180, 255, 219, 128, 128, 128,
      1, 185, 249, 255, 243, 255, 128, 128, 128, 128, 128,
    184, 150, 247, 255, 236, 224, 128, 128, 128, 128, 128,
     77, 110, 216, 255, 236, 230, 128, 128, 128, 128, 128,
      1, 101, 251, 255, 241, 255, 128, 128, 128, 128, 128,
    170, 139, 241, 252, 236, 209, 255, 255, 128, 128, 128,
     37, 116, 196, 243, 228, 255, 255, 255, 128, 128, 128,
      1, 204, 254, 255, 245, 255, 128, 128, 128, 128, 128,
    207, 160, 250, 255, 238, 128, 128, 128, 128, 128, 128,
    102, 103, 231, 255, 211, 171, 128, 128, 128, 128, 128,
      1, 152, 252, 255, 240, 255, 128, 128, 128, 128, 128,
    177, 135, 243, 255, 234, 225, 128, 128, 128, 128, 128,
     80, 129, 211, 255, 194, 224, 128, 128, 128, 128, 128,
      1,   1, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    246,   1, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    255, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    198,  35, 237, 223, 193, 187, 162, 160, 145, 155,  62,
    131,  45, 198, 221, 172, 176, 220, 157, 252, 221,   1,
     68,  47, 146, 208, 149, 167, 221, 162, 255, 223, 128,
      1, 149, 241, 255, 221, 224, 255, 255, 128, 128, 128,
    184, 141, 234, 253, 222, 220, 255, 199, 128, 128, 128,
     81,  99, 181, 242, 176, 190, 249, 202, 255, 255, 128,
      1, 129, 232, 253, 214, 197, 242, 196, 255, 255, 128,
     99, 121, 210, 250, 201, 198, 255, 202, 128, 128, 128,
     23,  91, 163, 242, 170, 187, 247, 210, 255, 255, 128,
      1, 200, 246, 255, 234, 255, 128, 128, 128, 128, 128,
    109, 178, 241, 255, 231, 245, 255, 255, 128, 128, 128,
     44, 130, 201, 253, 205, 192, 255, 255, 128, 128, 128,
      1, 132, 239, 251, 219, 209, 255, 165, 128, 128, 128,
     94, 136, 225, 251, 218, 190, 255, 255, 128, 128, 128,
     22, 100, 174, 245, 186, 161, 255, 199, 128, 128, 128,
      1, 182, 249, 255, 232, 235, 128, 128, 128, 128, 128,
    124, 143, 241, 255, 227, 234, 128, 128, 128, 128, 128,
     35,  77, 181, 251, 193, 211, 255, 205, 128, 128, 128,
      1, 157, 247, 255, 236, 231, 255, 255, 128, 128, 128,
    121, 141, 235, 255, 225, 227, 255, 255, 128, 128, 128,
     45,  99, 188, 251, 195, 217, 255, 224, 128, 128, 128,
      1,   1, 251, 255, 213, 255, 128, 128, 128, 128, 128,
    203,   1, 248, 255, 255, 128, 128, 128, 128, 128, 128,
    137,   1, 177, 255, 224, 255, 128, 128, 128, 128, 128,
    253,   9, 248, 251, 207, 208, 255, 192, 128, 128, 128,
    175,  13, 224, 243, 193, 185, 249, 198, 255, 255, 128,
     73,  17, 171, 221, 161, 179, 236, 167, 255, 234, 128,
      1,  95, 247, 253, 212, 183, 255, 255, 128, 128, 128,
    239,  90, 244, 250, 211, 209, 255, 255, 128, 128, 128,
    155,  77, 195, 248, 188, 195, 255, 255, 128, 128, 128,
      1,  24, 239, 251, 218, 219, 255, 205, 128, 128, 128,
    201,  51, 219, 255, 196, 186, 128, 128, 128, 128, 128,
     69,  46, 190, 239, 201, 218, 255, 228, 128, 128, 128,
      1, 191, 251, 255, 255, 128, 128, 128, 128, 128, 128,
    223, 165, 249, 255, 213, 255, 128, 128, 128, 128, 128,
    141, 124, 248, 255, 255, 128, 128, 128, 128, 128, 128,
      1,  16, 248, 255, 255, 128, 128, 128, 128, 128, 128,
    190,  36, 230, 255, 236, 255, 128, 128, 128, 128, 128,
    149,   1, 255, 128, 128, 128, 128, 128, 128, 128, 128,
      1, 226, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    247, 192, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    240, 128, 255, 128, 128, 128, 128, 128, 128, 128, 128,
      1, 134, 252, 255, 255, 128, 128, 128, 128, 128, 128,
    213,  62, 250, 255, 255, 128, 128, 128, 128, 128, 128,
     55,  93, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    202,  24, 213, 235, 186, 191, 220, 160, 240, 175, 255,
    126,  38, 182, 232, 169, 184, 228, 174, 255, 187, 128,
     61,  46, 138, 219, 151, 178, 240, 170, 255, 216, 128,
      1, 112, 230, 250, 199, 191, 247, 159, 255, 255, 128,
    166, 109, 228, 252, 211, 215, 255, 174, 128, 128, 128,
     39,  77, 162, 232, 172, 180, 245, 178, 255, 255, 128,
      1,  52, 220, 246, 198, 199, 249, 220, 255, 255, 128,
    124,  74, 191, 243, 183, 193, 250, 221, 255, 255, 128,
     24,  71, 130, 219, 154, 170, 243, 182, 255, 255, 128,
      1, 182, 225, 249, 219, 240, 255, 224, 128, 128, 128,
    149, 150, 226, 252, 216, 205, 255, 171, 128, 128, 128,
     28, 108, 170, 242, 183, 194, 254, 223, 255, 255, 128,
      1,  81, 230, 252, 204, 203, 255, 192, 128, 128, 128,
    123, 102, 209, 247, 188, 196, 255, 233, 128, 128, 128,
     20,  95, 153, 243, 164, 173, 255, 203, 128, 128, 128,
      1, 222, 248, 255, 216, 213, 128, 128, 128, 128, 128,
    168, 175, 246, 252, 235, 205, 255, 255, 128, 128, 128,
     47, 116, 215, 255, 211, 212, 255, 255, 128, 128, 128,
      1, 121, 236, 253, 212, 214, 255, 255, 128, 128, 128,
    141,  84, 213, 252, 201, 202, 255, 219, 128, 128, 128,
     42,  80, 160, 240, 162, 185, 255, 205, 128, 128, 128,
      1,   1, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    244,   1, 255, 128, 128, 128, 128, 128, 128, 128, 128,
    238,   1, 255, 128, 128, 128, 128, 128, 128, 128, 128,
};

static const uint8_t kCoeffUpdateProbs[4 * 8 * 3 * 11] = {
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    176, 246, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    223, 241, 252, 255, 255, 255, 255, 255, 255, 255, 255,
    249, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 244, 252, 255, 255, 255, 255, 255, 255, 255, 255,
    234, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 246, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    239, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 248, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    251, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    251, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 254, 253, 255, 254, 255, 255, 255, 255, 255, 255,
    250, 255, 254, 255, 254, 255, 255, 255, 255, 255, 255,
    254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    217, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    225, 252, 241, 253, 255, 255, 254, 255, 255, 255, 255,
    234, 250, 241, 250, 253, 255, 253, 254, 255, 255, 255,
    255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    223, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    238, 253, 254, 254, 255, 255, 255, 255, 255, 255, 255,
    255, 248, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    249, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 253, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    247, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    252, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 254, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    250, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    186, 251, 250, 255, 255, 255, 255, 255, 255, 255, 255,
    234, 251, 244, 254, 255, 255, 255, 255, 255, 255, 255,
    251, 251, 243, 253, 254, 255, 254, 255, 255, 255, 255,
    255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    236, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    251, 253, 253, 254, 254, 255, 255, 255, 255, 255, 255,
    255, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    248, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    250, 254, 252, 254, 255, 255, 255, 255, 255, 255, 255,
    248, 254, 249, 253, 255, 255, 255, 255, 255, 255, 255,
    255, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    246, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    252, 254, 251, 254, 254, 255, 255, 255, 255, 255, 255,
    255, 254, 252, 255, 255, 255, 255, 255, 255, 255, 255,
    248, 254, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    253, 255, 254, 254, 255, 255, 255, 255, 255, 255, 255,
    255, 251, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    245, 251, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    253, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 251, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    252, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 252, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    249, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 253, 255, 255, 255, 255, 255, 255, 255, 255,
    250, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
};

static const uint8_t kBModeProbs[10 * 10 * 9] = {
    231, 120,  48,  89, 115, 113, 120, 152, 112,
    152, 179,  64, 126, 170, 118,  46,  70,  95,
    175,  69, 143,  80,  85,  82,  72, 155, 103,
     56,  58,  10, 171, 218, 189,  17,  13, 152,
    114,  26,  17, 163,  44, 195,  21,  10, 173,
    121,  24,  80, 195,  26,  62,  44,  64,  85,
    144,  71,  10,  38, 171, 213, 144,  34,  26,
    170,  46,  55,  19, 136, 160,  33, 206,  71,
     63,  20,   8, 114, 114, 208,  12,   9, 226,
     81,  40,  11,  96, 182,  84,  29,  16,  36,
    134, 183,  89, 137,  98, 101, 106, 165, 148,
     72, 187, 100, 130, 157, 111,  32,  75,  80,
     66, 102, 167,  99,  74,  62,  40, 234, 128,
     41,  53,   9, 178, 241, 141,  26,   8, 107,
     74,  43,  26, 146,  73, 166,  49,  23, 157,
     65,  38, 105, 160,  51,  52,  31, 115, 128,
    104,  79,  12,  27, 217, 255,  87,  17,   7,
     87,  68,  71,  44, 114,  51,  15, 186,  23,
     47,  41,  14, 110, 182, 183,  21,  17, 194,
     66,  45,  25, 102, 197, 189,  23,  18,  22,
     88,  88, 147, 150,  42,  46,  45, 196, 205,
     43,  97, 183, 117,  85,  38,  35, 179,  61,
     39,  53, 200,  87,  26,  21,  43, 232, 171,
     56,  34,  51, 104, 114, 102,  29,  93,  77,
     39,  28,  85, 171,  58, 165,  90,  98,  64,
     34,  22, 116, 206,  23,  34,  43, 166,  73,
    107,  54,  32,  26,  51,   1,  81,  43,  31,
     68,  25, 106,  22,  64, 171,  36, 225, 114,
     34,  19,  21, 102, 132, 188,  16,  76, 124,
     62,  18,  78,  95,  85,  57,  50,  48,  51,
    193, 101,  35, 159, 215, 111,  89,  46, 111,
     60, 148,  31, 172, 219, 228,  21,  18, 111,
    112, 113,  77,  85, 179, 255,  38, 120, 114,
     40,  42,   1, 196, 245, 209,  10,  25, 109,
     88,  43,  29, 140, 166, 213,  37,  43, 154,
     61,  63,  30, 155,  67,  45,  68,   1, 209,
    100,  80,   8,  43, 154,   1,  51,  26,  71,
    142,  78,  78,  16, 255, 128,  34, 197, 171,
     41,  40,   5, 102, 211, 183,   4,   1, 221,
     51,  50,  17, 168, 209, 192,  23,  25,  82,
    138,  31,  36, 171,  27, 166,  38,  44, 229,
     67,  87,  58, 169,  82, 115,  26,  59, 179,
     63,  59,  90, 180,  59, 166,  93,  73, 154,
     40,  40,  21, 116, 143, 209,  34,  39, 175,
     47,  15,  16, 183,  34, 223,  49,  45, 183,
     46,  17,  33, 183,   6,  98,  15,  32, 183,
     57,  46,  22,  24, 128,   1,  54,  17,  37,
     65,  32,  73, 115,  28, 128,  23, 128, 205,
     40,   3,   9, 115,  51, 192,  18,   6, 223,
     87,  37,   9, 115,  59,  77,  64,  21,  47,
    104,  55,  44, 218,   9,  54,  53, 130, 226,
     64,  90,  70, 205,  40,  41,  23,  26,  57,
     54,  57, 112, 184,   5,  41,  38, 166, 213,
     30,  34,  26, 133, 152, 116,  10,  32, 134,
     39,  19,  53, 221,  26, 114,  32,  73, 255,
     31,   9,  65, 234,   2,  15,   1, 118,  73,
     75,  32,  12,  51, 192, 255, 160,  43,  51,
     88,  31,  35,  67, 102,  85,  55, 186,  85,
     56,  21,  23, 111,  59, 205,  45,  37, 192,
     55,  38,  70, 124,  73, 102,   1,  34,  98,
    125,  98,  42,  88, 104,  85, 117, 175,  82,
     95,  84,  53,  89, 128, 100, 113, 101,  45,
     75,  79, 123,  47,  51, 128,  81, 171,   1,
     57,  17,   5,  71, 102,  57,  53,  41,  49,
     38,  33,  13, 121,  57,  73,  26,   1,  85,
     41,  10,  67, 138,  77, 110,  90,  47, 114,
    115,  21,   2,  10, 102, 255, 166,  23,   6,
    101,  29,  16,  10,  85, 128, 101, 196,  26,
     57,  18,  10, 102, 102, 213,  34,  20,  43,
    117,  20,  15,  36, 163, 128,  68,   1,  26,
    102,  61,  71,  37,  34,  53,  31, 243, 192,
     69,  60,  71,  38,  73, 119,  28, 222,  37,
     68,  45, 128,  34,   1,  47,  11, 245, 171,
     62,  17,  19,  70, 146,  85,  55,  62,  70,
     37,  43,  37, 154, 100, 163,  85, 160,   1,
     63,   9,  92, 136,  28,  64,  32, 201,  85,
     75,  15,   9,   9,  64, 255, 184, 119,  16,
     86,   6,  28,   5,  64, 255,  25, 248,   1,
     56,   8,  17, 132, 137, 255,  55, 116, 128,
     58,  15,  20,  82, 135,  57,  26, 121,  40,
    164,  50,  31, 137, 154, 133,  25,  35, 218,
     51, 103,  44, 131, 131, 123,  31,   6, 158,
     86,  40,  64, 135, 148, 224,  45, 183, 128,
     22,  26,  17, 131, 240, 154,  14,   1, 209,
     45,  16,  21,  91,  64, 222,   7,   1, 197,
     56,  21,  39, 155,  60, 138,  23, 102, 213,
     83,  12,  13,  54, 192, 255,  68,  47,  28,
     85,  26,  85,  85, 128, 128,  32, 146, 171,
     18,  11,   7,  63, 144, 171,   4,   4, 246,
     35,  27,  10, 146, 174, 171,  12,  26, 128,
    190,  80,  35,  99, 180,  80, 126,  54,  45,
     85, 126,  47,  87, 176,  51,  41,  20,  32,
    101,  75, 128, 139, 118, 146, 116, 128,  85,
     56,  41,  15, 176, 236,  85,  37,   9,  62,
     71,  30,  17, 119, 118, 255,  17,  18, 138,
    101,  38,  60, 138,  55,  70,  43,  26, 142,
    146,  36,  19,  30, 171, 255,  97,  27,  20,
    138,  45,  61,  62, 219,   1,  81, 188,  64,
     32,  41,  20, 117, 151, 142,  20,  21, 163,
    112,  19,  12,  61, 195, 128,  48,   4,  24,
};

static const uint8_t kDcTable[128] = {
      4,   5,   6,   7,   8,   9,  10,  10,  11,  12,  13,  14,  15,  16,  17,  17,
     18,  19,  20,  20,  21,  21,  22,  22,  23,  23,  24,  25,  25,  26,  27,  28,
     29,  30,  31,  32,  33,  34,  35,  36,  37,  37,  38,  39,  40,  41,  42,  43,
     44,  45,  46,  46,  47,  48,  49,  50,  51,  52,  53,  54,  55,  56,  57,  58,
     59,  60,  61,  62,  63,  64,  65,  66,  67,  68,  69,  70,  71,  72,  73,  74,
     75,  76,  76,  77,  78,  79,  80,  81,  82,  83,  84,  85,  86,  87,  88,  89,
     91,  93,  95,  96,  98, 100, 101, 102, 104, 106, 108, 110, 112, 114, 116, 118,
    122, 124, 126, 128, 130, 132, 134, 136, 138, 140, 143, 145, 148, 151, 154, 157,
};

static const uint16_t kAcTable[128] = {
      4,   5,   6,   7,   8,   9,  10,  11,  12,  13,  14,  15,  16,  17,  18,  19,
     20,  21,  22,  23,  24,  25,  26,  27,  28,  29,  30,  31,  32,  33,  34,  35,
     36,  37,  38,  39,  40,  41,  42,  43,  44,  45,  46,  47,  48,  49,  50,  51,
     52,  53,  54,  55,  56,  57,  58,  60,  62,  64,  66,  68,  70,  72,  74,  76,
     78,  80,  82,  84,  86,  88,  90,  92,  94,  96,  98, 100, 102, 104, 106, 108,
    110, 112, 114, 116, 119, 122, 125, 128, 131, 134, 137, 140, 143, 146, 149, 152,
    155, 158, 161, 164, 167, 170, 173, 177, 181, 185, 189, 193, 197, 201, 205, 209,
    213, 217, 221, 225, 229, 234, 239, 245, 249, 254, 259, 264, 269, 274, 279, 284,
};

static const uint8_t kZigzag[16] = { 0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15 };
static const uint8_t kBands[17] = { 0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0 };
static const uint8_t kCat3[] = { 173, 148, 140, 0 };
static const uint8_t kCat4[] = { 176, 155, 140, 135, 0 };
static const uint8_t kCat5[] = { 180, 157, 141, 134, 130, 0 };
static const uint8_t kCat6[] = { 254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129, 0 };
static const uint8_t *const kCat3456[] = { kCat3, kCat4, kCat5, kCat6 };

typedef uint8_t band_probs_t[3][11];

/* --- boolean decoder ------------------------------------------------------- */

typedef struct {
    webp_cursor_t  cur;
    const uint8_t *p;
    const uint8_t *e;
    uint32_t       value;
    int            bits;
    uint32_t       range;
    uint8_t        eof;
} bool_dec_t;

/* Running past the end feeds one byte of zeros and then raises eof, the point
 * at which libwebp gives up on the stream. */
static void bd_load(bool_dec_t *b) {
    if (b->p == b->e) {
        size_t n;
        b->p = webp_cursor_refill(&b->cur, &n);
        b->e = b->p + n;
    }
    if (b->e - b->p >= 3) {
        b->value = (b->value << 24) | ((uint32_t)b->p[0] << 16) | ((uint32_t)b->p[1] << 8) |
                   b->p[2];
        b->p += 3;
        b->bits += 24;
    } else if (b->p < b->e) {
        b->value = (b->value << 8) | *b->p++;
        b->bits += 8;
    } else if (!b->eof) {
        b->value <<= 8;
        b->bits += 8;
        b->eof = 1;
    } else {
        b->bits = 0;
    }
}

static void bd_start(bool_dec_t *b) {
    b->p = b->e = NULL;
    b->value = 0;
    b->bits = -8;
    b->range = 254;
    b->eof = 0;
    bd_load(b);
}

static inline int bd_bit(bool_dec_t *b, int prob) {
    uint32_t range = b->range;
    if (b->bits < 0) bd_load(b);
    const int pos = b->bits;
    const uint32_t split = (range * (uint32_t)prob) >> 8;
    int bit;
    if ((b->value >> pos) > split) {
        range -= split;
        b->value -= (split + 1) << pos;
        bit = 1;
    } else {
        range = split + 1;
        bit = 0;
    }
    const int shift = 7 ^ (31 ^ __builtin_clz(range));
    range <<= shift;
    b->bits -= shift;
    b->range = range - 1;
    return bit;
}

static inline int bd_signed(bool_dec_t *b, int v) {
    if (b->bits < 0) bd_load(b);
    const int pos = b->bits;
    const uint32_t split = b->range >> 1;
    const uint32_t value = b->value >> pos;
    const int32_t mask = (int32_t)(split - value) >> 31;
    b->bits -= 1;
    b->range += (uint32_t)mask;
    b->range |= 1;
    b->value -= ((split + 1) & (uint32_t)mask) << pos;
    return (v ^ mask) - mask;
}

static uint32_t bd_value(bool_dec_t *b, int bits) {
    uint32_t v = 0;
    while (bits-- > 0) v |= (uint32_t)bd_bit(b, 0x80) << bits;
    return v;
}

static int bd_signed_value(bool_dec_t *b, int bits) {
    const int value = (int)bd_value(b, bits);
    return bd_bit(b, 0x80) ? -value : value;
}

typedef struct {
    uint8_t limit;
    uint8_t ilevel;
    uint8_t inner;
    uint8_t hev;
} finfo_t;

typedef struct {
    uint8_t segment;
    uint8_t skip;
    uint8_t is_i4x4;
    uint8_t uvmode;
    uint8_t imodes[16];
} mb_t;

struct webp_vp8 {
    webp_source_t *src;
    int width;
    int height;
    int mb_w;
    int mb_h;

    bool_dec_t  br;
    bool_dec_t  parts[8];
    int         num_parts;

    uint8_t update_map;
    uint8_t seg_probs[3];
    uint8_t filter_type;
    finfo_t fstrengths[4][2];
    int     dq[4][3][2];
    uint8_t use_skip;
    uint8_t skip_p;
    band_probs_t probs[4][8];
    const band_probs_t *bands[4][17];

    uint8_t *intra_t;
    uint8_t  intra_l[4];
    uint8_t *nz_t;
    uint8_t *nz_dc_t;
    uint8_t  nz_l;
    uint8_t  nz_dc_l;
    uint8_t *top;
    finfo_t *finfo;
    uint8_t *work;
    int16_t  coeffs[384];
    uint8_t  kinds[24];

    uint8_t *cache_y;
    uint8_t *cache_u;
    uint8_t *cache_v;
    int      y_stride;
    int      uv_stride;
    int      mb_y;
    int      y_final;
    int      uv_final;
    int      out_y;
};

enum { Y1 = 0, Y2 = 1, UV = 2 };

/* --- headers --------------------------------------------------------------- */

static int clip(int v, int max) {
    return v < 0 ? 0 : v > max ? max : v;
}

static void parse_quant(webp_vp8_t *d, bool_dec_t *br, const int8_t quantizer[4], bool use_segment,
                        bool absolute) {
    const int base_q = (int)bd_value(br, 7);
    int delta[5];
    for (int i = 0; i < 5; i++) delta[i] = bd_bit(br, 0x80) ? bd_signed_value(br, 4) : 0;
    for (int s = 0; s < 4; s++) {
        int q = base_q;
        if (use_segment) q = quantizer[s] + (absolute ? 0 : base_q);
        d->dq[s][Y1][0] = kDcTable[clip(q + delta[0], 127)];
        d->dq[s][Y1][1] = kAcTable[clip(q, 127)];
        d->dq[s][Y2][0] = kDcTable[clip(q + delta[1], 127)] * 2;
        d->dq[s][Y2][1] = (kAcTable[clip(q + delta[2], 127)] * 101581) >> 16;
        if (d->dq[s][Y2][1] < 8) d->dq[s][Y2][1] = 8;
        d->dq[s][UV][0] = kDcTable[clip(q + delta[3], 117)];
        d->dq[s][UV][1] = kAcTable[clip(q + delta[4], 127)];
    }
}

static void filter_strengths(webp_vp8_t *d, const int8_t strength[4], bool use_segment,
                             bool absolute, int level, int sharpness, bool use_lf_delta,
                             const int ref_delta[4], const int mode_delta[4]) {
    for (int s = 0; s < 4; s++) {
        int base = level;
        if (use_segment) base = strength[s] + (absolute ? 0 : level);
        for (int i4x4 = 0; i4x4 < 2; i4x4++) {
            finfo_t *info = &d->fstrengths[s][i4x4];
            int lv = base;
            if (use_lf_delta) {
                lv += ref_delta[0];
                if (i4x4) lv += mode_delta[0];
            }
            lv = clip(lv, 63);
            info->inner = (uint8_t)i4x4;
            if (lv == 0) {
                info->limit = 0;
                continue;
            }
            int ilevel = lv;
            if (sharpness > 0) {
                ilevel >>= sharpness > 4 ? 2 : 1;
                if (ilevel > 9 - sharpness) ilevel = 9 - sharpness;
            }
            if (ilevel < 1) ilevel = 1;
            info->ilevel = (uint8_t)ilevel;
            info->limit = (uint8_t)(2 * lv + ilevel);
            info->hev = lv >= 40 ? 2 : lv >= 15 ? 1 : 0;
        }
    }
}

static imgf_err_t parse_header(webp_vp8_t *d, size_t parts_pos, size_t parts_end,
                               uint32_t caps) {
    bool_dec_t *br = &d->br;
    bd_value(br, 2);

    int8_t quantizer[4] = {0}, strength[4] = {0};
    bool use_segment = bd_bit(br, 0x80);
    bool absolute = true;
    memset(d->seg_probs, 255, sizeof d->seg_probs);
    if (use_segment) {
        d->update_map = (uint8_t)bd_bit(br, 0x80);
        if (bd_bit(br, 0x80)) {
            absolute = bd_bit(br, 0x80);
            for (int s = 0; s < 4; s++) {
                quantizer[s] = (int8_t)(bd_bit(br, 0x80) ? bd_signed_value(br, 7) : 0);
            }
            for (int s = 0; s < 4; s++) {
                strength[s] = (int8_t)(bd_bit(br, 0x80) ? bd_signed_value(br, 6) : 0);
            }
        }
        if (d->update_map) {
            for (int i = 0; i < 3; i++) {
                d->seg_probs[i] = (uint8_t)(bd_bit(br, 0x80) ? bd_value(br, 8) : 255);
            }
        }
    }

    const bool simple = bd_bit(br, 0x80);
    const int level = (int)bd_value(br, 6);
    const int sharpness = (int)bd_value(br, 3);
    const bool use_lf_delta = bd_bit(br, 0x80);
    int ref_delta[4] = {0}, mode_delta[4] = {0};
    if (use_lf_delta && bd_bit(br, 0x80)) {
        for (int i = 0; i < 4; i++) if (bd_bit(br, 0x80)) ref_delta[i] = bd_signed_value(br, 6);
        for (int i = 0; i < 4; i++) if (bd_bit(br, 0x80)) mode_delta[i] = bd_signed_value(br, 6);
    }
    d->filter_type = level == 0 ? 0 : simple ? 1 : 2;
    filter_strengths(d, strength, use_segment, absolute, level, sharpness, use_lf_delta,
                     ref_delta, mode_delta);

    d->num_parts = 1 << bd_value(br, 2);
    uint8_t sizes[21];
    const size_t table = 3 * (size_t)(d->num_parts - 1);
    if (parts_end - parts_pos < table) return IMGF_ERR_DECODE;
    if (webp_source_read(d->src, parts_pos, sizes, table) != table) return IMGF_ERR_TRUNCATED;
    size_t pos = parts_pos + table;
    for (int p = 0; p < d->num_parts; p++) {
        size_t len = parts_end - pos;
        if (p < d->num_parts - 1) {
            const size_t want = (size_t)sizes[3 * p] | ((size_t)sizes[3 * p + 1] << 8) |
                                ((size_t)sizes[3 * p + 2] << 16);
            if (want < len) len = want;
        } else if (len == 0) {
            return IMGF_ERR_DECODE;
        }
        if (!webp_cursor_init(&d->parts[p].cur, d->src, pos, len, TOKEN_WINDOW, caps)) {
            return IMGF_ERR_OOM;
        }
        pos += len;
    }

    parse_quant(d, br, quantizer, use_segment, absolute);
    bd_bit(br, 0x80);

    for (int t = 0; t < 4; t++) {
        for (int b = 0; b < 8; b++) {
            for (int c = 0; c < 3; c++) {
                for (int p = 0; p < 11; p++) {
                    const int i = ((t * 8 + b) * 3 + c) * 11 + p;
                    d->probs[t][b][c][p] =
                        (uint8_t)(bd_bit(br, kCoeffUpdateProbs[i]) ? bd_value(br, 8) : kCoeffProbs[i]);
                }
            }
        }
        for (int n = 0; n < 17; n++) d->bands[t][n] = &d->probs[t][kBands[n]];
    }
    d->use_skip = (uint8_t)bd_bit(br, 0x80);
    if (d->use_skip) d->skip_p = (uint8_t)bd_value(br, 8);
    return br->eof ? webp_eof_error(d->src) : IMGF_OK;
}

/* --- modes and coefficients ------------------------------------------------ */

static void parse_modes(webp_vp8_t *d, int mb_x, mb_t *mb) {
    bool_dec_t *br = &d->br;
    uint8_t *top = d->intra_t + 4 * mb_x;
    uint8_t *left = d->intra_l;
    if (d->update_map) {
        mb->segment = (uint8_t)(!bd_bit(br, d->seg_probs[0]) ? bd_bit(br, d->seg_probs[1])
                                                             : 2 + bd_bit(br, d->seg_probs[2]));
    } else {
        mb->segment = 0;
    }
    mb->skip = (uint8_t)(d->use_skip ? bd_bit(br, d->skip_p) : 0);
    mb->is_i4x4 = (uint8_t)!bd_bit(br, 145);
    if (!mb->is_i4x4) {
        const int ymode = bd_bit(br, 156) ? (bd_bit(br, 128) ? TM_PRED : H_PRED)
                                          : (bd_bit(br, 163) ? V_PRED : DC_PRED);
        mb->imodes[0] = (uint8_t)ymode;
        memset(top, ymode, 4);
        memset(left, ymode, 4);
    } else {
        for (int y = 0; y < 4; y++) {
            int ymode = left[y];
            for (int x = 0; x < 4; x++) {
                const uint8_t *p = kBModeProbs + (top[x] * 10 + ymode) * 9;
                if (!bd_bit(br, p[0])) {
                    ymode = B_DC_PRED;
                } else if (!bd_bit(br, p[1])) {
                    ymode = B_TM_PRED;
                } else if (!bd_bit(br, p[2])) {
                    ymode = B_VE_PRED;
                } else if (!bd_bit(br, p[3])) {
                    ymode = !bd_bit(br, p[4]) ? B_HE_PRED : !bd_bit(br, p[5]) ? B_RD_PRED : B_VR_PRED;
                } else {
                    ymode = !bd_bit(br, p[6])   ? B_LD_PRED
                            : !bd_bit(br, p[7]) ? B_VL_PRED
                            : !bd_bit(br, p[8]) ? B_HD_PRED
                                                : B_HU_PRED;
                }
                top[x] = (uint8_t)ymode;
                mb->imodes[y * 4 + x] = (uint8_t)ymode;
            }
            left[y] = (uint8_t)ymode;
        }
    }
    mb->uvmode = !bd_bit(br, 142)   ? DC_PRED
                 : !bd_bit(br, 114) ? V_PRED
                 : bd_bit(br, 183)  ? TM_PRED
                                    : H_PRED;
}

static int large_value(bool_dec_t *br, const uint8_t *p) {
    if (!bd_bit(br, p[3])) {
        if (!bd_bit(br, p[4])) return 2;
        return 3 + bd_bit(br, p[5]);
    }
    if (!bd_bit(br, p[6])) {
        if (!bd_bit(br, p[7])) return 5 + bd_bit(br, 159);
        int v = 7 + 2 * bd_bit(br, 165);
        return v + bd_bit(br, 145);
    }
    const int bit1 = bd_bit(br, p[8]);
    const int bit0 = bd_bit(br, p[9 + bit1]);
    const int cat = 2 * bit1 + bit0;
    int v = 0;
    for (const uint8_t *tab = kCat3456[cat]; *tab; tab++) v += v + bd_bit(br, *tab);
    return v + 3 + (8 << cat);
}

/* Returns one past the last coefficient read. Values are stored the way
 * libwebp stores them, truncated to 16 bits after dequantisation. */
static int get_coeffs(bool_dec_t *br, const band_probs_t *const *bands, int ctx, const int dq[2],
                      int n, int16_t *out) {
    const uint8_t *p = (*bands[n])[ctx];
    for (; n < 16; n++) {
        if (!bd_bit(br, p[0])) return n;
        while (!bd_bit(br, p[1])) {
            p = (*bands[++n])[0];
            if (n == 16) return 16;
        }
        const band_probs_t *next = bands[n + 1];
        int v;
        if (!bd_bit(br, p[2])) {
            v = 1;
            p = (*next)[1];
        } else {
            v = large_value(br, p);
            p = (*next)[2];
        }
        out[kZigzag[n]] = (int16_t)(bd_signed(br, v) * dq[n > 0]);
    }
    return 16;
}

static void wht(const int16_t *in, int16_t *out) {
    int tmp[16];
    for (int i = 0; i < 4; i++) {
        const int a0 = in[0 + i] + in[12 + i];
        const int a1 = in[4 + i] + in[8 + i];
        const int a2 = in[4 + i] - in[8 + i];
        const int a3 = in[0 + i] - in[12 + i];
        tmp[0 + i] = a0 + a1;
        tmp[8 + i] = a0 - a1;
        tmp[4 + i] = a3 + a2;
        tmp[12 + i] = a3 - a2;
    }
    for (int i = 0; i < 4; i++) {
        const int dc = tmp[0 + i * 4] + 3;
        const int a0 = dc + tmp[3 + i * 4];
        const int a1 = tmp[1 + i * 4] + tmp[2 + i * 4];
        const int a2 = tmp[1 + i * 4] - tmp[2 + i * 4];
        const int a3 = dc - tmp[3 + i * 4];
        out[0] = (int16_t)((a0 + a1) >> 3);
        out[16] = (int16_t)((a3 + a2) >> 3);
        out[32] = (int16_t)((a0 - a1) >> 3);
        out[48] = (int16_t)((a3 - a2) >> 3);
        out += 64;
    }
}

static uint8_t block_kind(int nz, const int16_t *block) {
    return nz > 1 ? 2 : block[0] != 0;
}

/* Returns true when the macroblock has no coefficient at all. */
static bool parse_residuals(webp_vp8_t *d, int mb_x, const mb_t *mb, bool_dec_t *br) {
    const band_probs_t *const *ac_bands;
    const int (*dq)[2] = d->dq[mb->segment];
    int16_t *dst = d->coeffs;
    int first;
    bool any = false;

    memset(dst, 0, sizeof d->coeffs);
    if (!mb->is_i4x4) {
        int16_t dc[16] = {0};
        const int ctx = d->nz_dc_t[mb_x] + d->nz_dc_l;
        const int nz = get_coeffs(br, d->bands[1], ctx, dq[Y2], 0, dc);
        d->nz_dc_t[mb_x] = d->nz_dc_l = nz > 0;
        if (nz > 1) {
            wht(dc, dst);
        } else {
            const int16_t dc0 = (int16_t)((dc[0] + 3) >> 3);
            for (int i = 0; i < 256; i += 16) dst[i] = dc0;
        }
        first = 1;
        ac_bands = d->bands[0];
    } else {
        first = 0;
        ac_bands = d->bands[3];
    }

    uint8_t tnz = d->nz_t[mb_x];
    uint8_t lnz = d->nz_l;
    for (int y = 0; y < 4; y++) {
        int l = (lnz >> y) & 1;
        for (int x = 0; x < 4; x++) {
            const int ctx = l + ((tnz >> x) & 1);
            int16_t *block = dst + (y * 4 + x) * 16;
            const int nz = get_coeffs(br, ac_bands, ctx, dq[Y1], first, block);
            l = nz > first;
            tnz = (uint8_t)((tnz & ~(1 << x)) | (l << x));
            d->kinds[y * 4 + x] = block_kind(nz, block);
            any |= d->kinds[y * 4 + x] != 0;
        }
        lnz = (uint8_t)((lnz & ~(1 << y)) | (l << y));
    }
    for (int ch = 0; ch < 2; ch++) {
        const int shift = 4 + 2 * ch;
        for (int y = 0; y < 2; y++) {
            int l = (lnz >> (shift + y)) & 1;
            for (int x = 0; x < 2; x++) {
                const int ctx = l + ((tnz >> (shift + x)) & 1);
                const int b = 16 + ch * 4 + y * 2 + x;
                int16_t *block = dst + b * 16;
                const int nz = get_coeffs(br, d->bands[2], ctx, dq[UV], 0, block);
                l = nz > 0;
                tnz = (uint8_t)((tnz & ~(1 << (shift + x))) | (l << (shift + x)));
                d->kinds[b] = block_kind(nz, block);
                any |= d->kinds[b] != 0;
            }
            lnz = (uint8_t)((lnz & ~(1 << (shift + y))) | (l << (shift + y)));
        }
    }
    d->nz_t[mb_x] = tnz;
    d->nz_l = lnz;
    return !any;
}

/* --- reconstruction -------------------------------------------------------- */

static inline uint8_t clip_8b(int v) {
    return (uint8_t)(!(v & ~0xff) ? v : v < 0 ? 0 : 255);
}

#define MUL1(a) ((((a) * 20091) >> 16) + (a))
#define MUL2(a) (((a) * 35468) >> 16)

static void idct_add(const int16_t *in, uint8_t *dst) {
    int tmp[16];
    for (int i = 0; i < 4; i++) {
        const int a = in[0] + in[8];
        const int b = in[0] - in[8];
        const int c = MUL2(in[4]) - MUL1(in[12]);
        const int e = MUL1(in[4]) + MUL2(in[12]);
        tmp[4 * i + 0] = a + e;
        tmp[4 * i + 1] = b + c;
        tmp[4 * i + 2] = b - c;
        tmp[4 * i + 3] = a - e;
        in++;
    }
    for (int i = 0; i < 4; i++) {
        const int dc = tmp[i] + 4;
        const int a = dc + tmp[8 + i];
        const int b = dc - tmp[8 + i];
        const int c = MUL2(tmp[4 + i]) - MUL1(tmp[12 + i]);
        const int e = MUL1(tmp[4 + i]) + MUL2(tmp[12 + i]);
        dst[0] = clip_8b(dst[0] + ((a + e) >> 3));
        dst[1] = clip_8b(dst[1] + ((b + c) >> 3));
        dst[2] = clip_8b(dst[2] + ((b - c) >> 3));
        dst[3] = clip_8b(dst[3] + ((a - e) >> 3));
        dst += BPS;
    }
}

static void dc_add(const int16_t *in, uint8_t *dst) {
    const int dc = in[0] + 4;
    for (int y = 0; y < 4; y++, dst += BPS) {
        for (int x = 0; x < 4; x++) dst[x] = clip_8b(dst[x] + (dc >> 3));
    }
}

static void add_block(uint8_t kind, const int16_t *in, uint8_t *dst) {
    if (kind == 2) {
        idct_add(in, dst);
    } else if (kind == 1) {
        dc_add(in, dst);
    }
}

#define AVG3(a, b, c) ((uint8_t)(((a) + 2 * (b) + (c) + 2) >> 2))
#define AVG2(a, b) ((uint8_t)(((a) + (b) + 1) >> 1))
#define DST(x, y) dst[(x) + (y) * BPS]

static void true_motion(uint8_t *dst, int size) {
    const uint8_t *top = dst - BPS;
    for (int y = 0; y < size; y++, dst += BPS) {
        const int left = dst[-1] - top[-1];
        for (int x = 0; x < size; x++) dst[x] = clip_8b(top[x] + left);
    }
}

static void fill(uint8_t *dst, int size, int v) {
    for (int y = 0; y < size; y++) memset(dst + y * BPS, v, (size_t)size);
}

static void predict_block(uint8_t *dst, int size, int mode) {
    const int log2 = size == 16 ? 4 : 3;
    int dc;
    switch (mode) {
    case TM_PRED:
        true_motion(dst, size);
        return;
    case V_PRED:
        for (int y = 0; y < size; y++) memcpy(dst + y * BPS, dst - BPS, (size_t)size);
        return;
    case H_PRED:
        for (int y = 0; y < size; y++) memset(dst + y * BPS, dst[y * BPS - 1], (size_t)size);
        return;
    case DC_PRED:
        dc = size;
        for (int i = 0; i < size; i++) dc += dst[i - BPS] + dst[i * BPS - 1];
        fill(dst, size, dc >> (log2 + 1));
        return;
    case DC_NOTOP:
        dc = size >> 1;
        for (int i = 0; i < size; i++) dc += dst[i * BPS - 1];
        fill(dst, size, dc >> log2);
        return;
    case DC_NOLEFT:
        dc = size >> 1;
        for (int i = 0; i < size; i++) dc += dst[i - BPS];
        fill(dst, size, dc >> log2);
        return;
    default:
        fill(dst, size, 0x80);
        return;
    }
}

static void predict4(uint8_t *dst, int mode) {
    const uint8_t *top = dst - BPS;
    const int X = top[-1], A = top[0], B = top[1], C = top[2], D = top[3];
    const int E = top[4], F = top[5], G = top[6], H = top[7];
    const int I = dst[-1], J = dst[BPS - 1], K = dst[2 * BPS - 1], L = dst[3 * BPS - 1];
    switch (mode) {
    case B_DC_PRED: {
        int dc = 4;
        for (int i = 0; i < 4; i++) dc += top[i] + dst[i * BPS - 1];
        fill(dst, 4, dc >> 3);
        break;
    }
    case B_TM_PRED:
        true_motion(dst, 4);
        break;
    case B_VE_PRED: {
        const uint8_t v[4] = { AVG3(X, A, B), AVG3(A, B, C), AVG3(B, C, D), AVG3(C, D, E) };
        for (int y = 0; y < 4; y++) memcpy(dst + y * BPS, v, 4);
        break;
    }
    case B_HE_PRED:
        memset(dst, AVG3(X, I, J), 4);
        memset(dst + BPS, AVG3(I, J, K), 4);
        memset(dst + 2 * BPS, AVG3(J, K, L), 4);
        memset(dst + 3 * BPS, AVG3(K, L, L), 4);
        break;
    case B_RD_PRED:
        DST(0, 3) = AVG3(J, K, L);
        DST(1, 3) = DST(0, 2) = AVG3(I, J, K);
        DST(2, 3) = DST(1, 2) = DST(0, 1) = AVG3(X, I, J);
        DST(3, 3) = DST(2, 2) = DST(1, 1) = DST(0, 0) = AVG3(A, X, I);
        DST(3, 2) = DST(2, 1) = DST(1, 0) = AVG3(B, A, X);
        DST(3, 1) = DST(2, 0) = AVG3(C, B, A);
        DST(3, 0) = AVG3(D, C, B);
        break;
    case B_LD_PRED:
        DST(0, 0) = AVG3(A, B, C);
        DST(1, 0) = DST(0, 1) = AVG3(B, C, D);
        DST(2, 0) = DST(1, 1) = DST(0, 2) = AVG3(C, D, E);
        DST(3, 0) = DST(2, 1) = DST(1, 2) = DST(0, 3) = AVG3(D, E, F);
        DST(3, 1) = DST(2, 2) = DST(1, 3) = AVG3(E, F, G);
        DST(3, 2) = DST(2, 3) = AVG3(F, G, H);
        DST(3, 3) = AVG3(G, H, H);
        break;
    case B_VR_PRED:
        DST(0, 0) = DST(1, 2) = AVG2(X, A);
        DST(1, 0) = DST(2, 2) = AVG2(A, B);
        DST(2, 0) = DST(3, 2) = AVG2(B, C);
        DST(3, 0) = AVG2(C, D);
        DST(0, 3) = AVG3(K, J, I);
        DST(0, 2) = AVG3(J, I, X);
        DST(0, 1) = DST(1, 3) = AVG3(I, X, A);
        DST(1, 1) = DST(2, 3) = AVG3(X, A, B);
        DST(2, 1) = DST(3, 3) = AVG3(A, B, C);
        DST(3, 1) = AVG3(B, C, D);
        break;
    case B_VL_PRED:
        DST(0, 0) = AVG2(A, B);
        DST(1, 0) = DST(0, 2) = AVG2(B, C);
        DST(2, 0) = DST(1, 2) = AVG2(C, D);
        DST(3, 0) = DST(2, 2) = AVG2(D, E);
        DST(0, 1) = AVG3(A, B, C);
        DST(1, 1) = DST(0, 3) = AVG3(B, C, D);
        DST(2, 1) = DST(1, 3) = AVG3(C, D, E);
        DST(3, 1) = DST(2, 3) = AVG3(D, E, F);
        DST(3, 2) = AVG3(E, F, G);
        DST(3, 3) = AVG3(F, G, H);
        break;
    case B_HD_PRED:
        DST(0, 0) = DST(2, 1) = AVG2(I, X);
        DST(0, 1) = DST(2, 2) = AVG2(J, I);
        DST(0, 2) = DST(2, 3) = AVG2(K, J);
        DST(0, 3) = AVG2(L, K);
        DST(3, 0) = AVG3(A, B, C);
        DST(2, 0) = AVG3(X, A, B);
        DST(1, 0) = DST(3, 1) = AVG3(I, X, A);
        DST(1, 1) = DST(3, 2) = AVG3(J, I, X);
        DST(1, 2) = DST(3, 3) = AVG3(K, J, I);
        DST(1, 3) = AVG3(L, K, J);
        break;
    default:
        DST(0, 0) = AVG2(I, J);
        DST(2, 0) = DST(0, 1) = AVG2(J, K);
        DST(2, 1) = DST(0, 2) = AVG2(K, L);
        DST(1, 0) = AVG3(I, J, K);
        DST(3, 0) = DST(1, 1) = AVG3(J, K, L);
        DST(3, 1) = DST(1, 2) = AVG3(K, L, L);
        DST(3, 2) = DST(2, 2) = DST(0, 3) = DST(1, 3) = DST(2, 3) = DST(3, 3) = (uint8_t)L;
        break;
    }
}

static int edge_mode(int mb_x, int mb_y, int mode) {
    if (mode != DC_PRED) return mode;
    if (mb_x == 0) return mb_y == 0 ? DC_NOTOPLEFT : DC_NOLEFT;
    return mb_y == 0 ? DC_NOTOP : DC_PRED;
}

/* The work block keeps the left neighbour's last columns and the unfiltered
 * row above, which is what intra prediction reads; the loop filter only ever
 * touches the cache. */
static void reconstruct(webp_vp8_t *d, int mb_x, const mb_t *mb) {
    const int mb_y = d->mb_y;
    uint8_t *const y_dst = d->work + Y_OFF;
    uint8_t *const u_dst = d->work + U_OFF;
    uint8_t *const v_dst = d->work + V_OFF;
    uint8_t *const top = d->top + mb_x * 32;

    if (mb_x == 0) {
        for (int j = 0; j < 16; j++) y_dst[j * BPS - 1] = 129;
        for (int j = 0; j < 8; j++) u_dst[j * BPS - 1] = v_dst[j * BPS - 1] = 129;
        if (mb_y > 0) {
            y_dst[-1 - BPS] = u_dst[-1 - BPS] = v_dst[-1 - BPS] = 129;
        } else {
            memset(y_dst - BPS - 1, 127, 16 + 4 + 1);
            memset(u_dst - BPS - 1, 127, 8 + 1);
            memset(v_dst - BPS - 1, 127, 8 + 1);
        }
    } else {
        for (int j = -1; j < 16; j++) memcpy(y_dst + j * BPS - 4, y_dst + j * BPS + 12, 4);
        for (int j = -1; j < 8; j++) {
            memcpy(u_dst + j * BPS - 4, u_dst + j * BPS + 4, 4);
            memcpy(v_dst + j * BPS - 4, v_dst + j * BPS + 4, 4);
        }
    }
    if (mb_y > 0) {
        memcpy(y_dst - BPS, top, 16);
        memcpy(u_dst - BPS, top + 16, 8);
        memcpy(v_dst - BPS, top + 24, 8);
    }

    if (mb->is_i4x4) {
        uint8_t *const top_right = y_dst - BPS + 16;
        if (mb_y > 0) {
            if (mb_x >= d->mb_w - 1) {
                memset(top_right, top[15], 4);
            } else {
                memcpy(top_right, top + 32, 4);
            }
        }
        for (int k = 1; k < 4; k++) memcpy(top_right + 4 * k * BPS, top_right, 4);
        for (int n = 0; n < 16; n++) {
            uint8_t *const dst = y_dst + (n >> 2) * 4 * BPS + (n & 3) * 4;
            predict4(dst, mb->imodes[n]);
            add_block(d->kinds[n], d->coeffs + n * 16, dst);
        }
    } else {
        predict_block(y_dst, 16, edge_mode(mb_x, mb_y, mb->imodes[0]));
        for (int n = 0; n < 16; n++) {
            add_block(d->kinds[n], d->coeffs + n * 16, y_dst + (n >> 2) * 4 * BPS + (n & 3) * 4);
        }
    }
    const int uvmode = edge_mode(mb_x, mb_y, mb->uvmode);
    predict_block(u_dst, 8, uvmode);
    predict_block(v_dst, 8, uvmode);
    for (int n = 0; n < 4; n++) {
        const int off = (n >> 1) * 4 * BPS + (n & 1) * 4;
        add_block(d->kinds[16 + n], d->coeffs + (16 + n) * 16, u_dst + off);
        add_block(d->kinds[20 + n], d->coeffs + (20 + n) * 16, v_dst + off);
    }

    memcpy(top, y_dst + 15 * BPS, 16);
    memcpy(top + 16, u_dst + 7 * BPS, 8);
    memcpy(top + 24, v_dst + 7 * BPS, 8);

    uint8_t *const y_out = d->cache_y + CACHE_ABOVE * d->y_stride + mb_x * 16;
    uint8_t *const u_out = d->cache_u + CACHE_ABOVE * d->uv_stride + mb_x * 8;
    uint8_t *const v_out = d->cache_v + CACHE_ABOVE * d->uv_stride + mb_x * 8;
    for (int j = 0; j < 16; j++) memcpy(y_out + j * d->y_stride, y_dst + j * BPS, 16);
    for (int j = 0; j < 8; j++) {
        memcpy(u_out + j * d->uv_stride, u_dst + j * BPS, 8);
        memcpy(v_out + j * d->uv_stride, v_dst + j * BPS, 8);
    }
}

/* --- loop filter ----------------------------------------------------------- */

static inline int abs_i(int v) { return v < 0 ? -v : v; }
static inline int sclip1(int v) { return v < -128 ? -128 : v > 127 ? 127 : v; }
static inline int sclip2(int v) { return v < -16 ? -16 : v > 15 ? 15 : v; }

static inline void filter2(uint8_t *p, int step) {
    const int p1 = p[-2 * step], p0 = p[-step], q0 = p[0], q1 = p[step];
    const int a = 3 * (q0 - p0) + sclip1(p1 - q1);
    const int a1 = sclip2((a + 4) >> 3);
    const int a2 = sclip2((a + 3) >> 3);
    p[-step] = clip_8b(p0 + a2);
    p[0] = clip_8b(q0 - a1);
}

static inline void filter4(uint8_t *p, int step) {
    const int p1 = p[-2 * step], p0 = p[-step], q0 = p[0], q1 = p[step];
    const int a = 3 * (q0 - p0);
    const int a1 = sclip2((a + 4) >> 3);
    const int a2 = sclip2((a + 3) >> 3);
    const int a3 = (a1 + 1) >> 1;
    p[-2 * step] = clip_8b(p1 + a3);
    p[-step] = clip_8b(p0 + a2);
    p[0] = clip_8b(q0 - a1);
    p[step] = clip_8b(q1 - a3);
}

static inline void filter6(uint8_t *p, int step) {
    const int p2 = p[-3 * step], p1 = p[-2 * step], p0 = p[-step];
    const int q0 = p[0], q1 = p[step], q2 = p[2 * step];
    const int a = sclip1(3 * (q0 - p0) + sclip1(p1 - q1));
    const int a1 = (27 * a + 63) >> 7;
    const int a2 = (18 * a + 63) >> 7;
    const int a3 = (9 * a + 63) >> 7;
    p[-3 * step] = clip_8b(p2 + a3);
    p[-2 * step] = clip_8b(p1 + a2);
    p[-step] = clip_8b(p0 + a1);
    p[0] = clip_8b(q0 - a1);
    p[step] = clip_8b(q1 - a2);
    p[2 * step] = clip_8b(q2 - a3);
}

static inline bool hev(const uint8_t *p, int step, int thresh) {
    return abs_i(p[-2 * step] - p[-step]) > thresh || abs_i(p[step] - p[0]) > thresh;
}

static inline bool needs_filter(const uint8_t *p, int step, int t) {
    return 4 * abs_i(p[-step] - p[0]) + abs_i(p[-2 * step] - p[step]) <= t;
}

static inline bool needs_filter2(const uint8_t *p, int step, int t, int it) {
    const int p3 = p[-4 * step], p2 = p[-3 * step], p1 = p[-2 * step], p0 = p[-step];
    const int q0 = p[0], q1 = p[step], q2 = p[2 * step], q3 = p[3 * step];
    if (4 * abs_i(p0 - q0) + abs_i(p1 - q1) > t) return false;
    return abs_i(p3 - p2) <= it && abs_i(p2 - p1) <= it && abs_i(p1 - p0) <= it &&
           abs_i(q3 - q2) <= it && abs_i(q2 - q1) <= it && abs_i(q1 - q0) <= it;
}

static void simple_edge(uint8_t *p, int step, int along, int thresh) {
    const int t = 2 * thresh + 1;
    for (int i = 0; i < 16; i++, p += along) {
        if (needs_filter(p, step, t)) filter2(p, step);
    }
}

static void edge(uint8_t *p, int step, int along, int size, int thresh, int ithresh, int hev_t,
                 bool mb_edge) {
    const int t = 2 * thresh + 1;
    for (int i = 0; i < size; i++, p += along) {
        if (!needs_filter2(p, step, t, ithresh)) continue;
        if (hev(p, step, hev_t)) {
            filter2(p, step);
        } else if (mb_edge) {
            filter6(p, step);
        } else {
            filter4(p, step);
        }
    }
}

static void filter_mb(webp_vp8_t *d, int mb_x) {
    const finfo_t *f = &d->finfo[mb_x];
    const int limit = f->limit;
    if (limit == 0) return;
    const int ys = d->y_stride;
    uint8_t *y = d->cache_y + CACHE_ABOVE * ys + mb_x * 16;
    if (d->filter_type == 1) {
        if (mb_x > 0) simple_edge(y, 1, ys, limit + 4);
        if (f->inner) for (int k = 4; k < 16; k += 4) simple_edge(y + k, 1, ys, limit);
        if (d->mb_y > 0) simple_edge(y, ys, 1, limit + 4);
        if (f->inner) for (int k = 4; k < 16; k += 4) simple_edge(y + k * ys, ys, 1, limit);
        return;
    }
    const int uvs = d->uv_stride;
    uint8_t *u = d->cache_u + CACHE_ABOVE * uvs + mb_x * 8;
    uint8_t *v = d->cache_v + CACHE_ABOVE * uvs + mb_x * 8;
    const int il = f->ilevel, ht = f->hev;
    if (mb_x > 0) {
        edge(y, 1, ys, 16, limit + 4, il, ht, true);
        edge(u, 1, uvs, 8, limit + 4, il, ht, true);
        edge(v, 1, uvs, 8, limit + 4, il, ht, true);
    }
    if (f->inner) {
        for (int k = 4; k < 16; k += 4) edge(y + k, 1, ys, 16, limit, il, ht, false);
        edge(u + 4, 1, uvs, 8, limit, il, ht, false);
        edge(v + 4, 1, uvs, 8, limit, il, ht, false);
    }
    if (d->mb_y > 0) {
        edge(y, ys, 1, 16, limit + 4, il, ht, true);
        edge(u, uvs, 1, 8, limit + 4, il, ht, true);
        edge(v, uvs, 1, 8, limit + 4, il, ht, true);
    }
    if (f->inner) {
        for (int k = 4; k < 16; k += 4) edge(y + k * ys, ys, 1, 16, limit, il, ht, false);
        edge(u + 4 * uvs, uvs, 1, 8, limit, il, ht, false);
        edge(v + 4 * uvs, uvs, 1, 8, limit, il, ht, false);
    }
}

/* --- rows ------------------------------------------------------------------ */

static imgf_err_t decode_mb_row(webp_vp8_t *d) {
    if (d->mb_y > 0) {
        memcpy(d->cache_y, d->cache_y + 16 * d->y_stride, (size_t)CACHE_ABOVE * d->y_stride);
        memcpy(d->cache_u, d->cache_u + 8 * d->uv_stride, (size_t)CACHE_ABOVE * d->uv_stride);
        memcpy(d->cache_v, d->cache_v + 8 * d->uv_stride, (size_t)CACHE_ABOVE * d->uv_stride);
    }
    bool_dec_t *tokens = &d->parts[d->mb_y & (d->num_parts - 1)];
    d->nz_l = 0;
    d->nz_dc_l = 0;
    memset(d->intra_l, B_DC_PRED, sizeof d->intra_l);
    for (int mb_x = 0; mb_x < d->mb_w; mb_x++) {
        mb_t mb;
        parse_modes(d, mb_x, &mb);
        bool skip = d->use_skip && mb.skip;
        if (!skip) {
            skip = parse_residuals(d, mb_x, &mb, tokens);
        } else {
            d->nz_t[mb_x] = d->nz_l = 0;
            if (!mb.is_i4x4) d->nz_dc_t[mb_x] = d->nz_dc_l = 0;
            memset(d->kinds, 0, sizeof d->kinds);
        }
        if (tokens->eof) return webp_eof_error(d->src);
        d->finfo[mb_x] = d->fstrengths[mb.segment][mb.is_i4x4];
        d->finfo[mb_x].inner |= !skip;
        reconstruct(d, mb_x, &mb);
    }
    if (d->br.eof) return webp_eof_error(d->src);
    if (d->filter_type) {
        for (int mb_x = 0; mb_x < d->mb_w; mb_x++) filter_mb(d, mb_x);
    }

    d->mb_y++;
    if (d->mb_y == d->mb_h) {
        d->y_final = d->height;
        d->uv_final = (d->height + 1) >> 1;
    } else {
        const int y_delay = d->filter_type == 2 ? 3 : d->filter_type == 1 ? 1 : 0;
        d->y_final = d->mb_y * 16 - y_delay;
        d->uv_final = d->mb_y * 8 - (d->filter_type == 2 ? 3 : 0);
    }
    return IMGF_OK;
}

static int chroma_needed(const webp_vp8_t *d, int y) {
    if (y == 0) return 0;
    const int far = (y & 1) ? (y + 1) >> 1 : (y >> 1) - 1;
    const int near = (y & 1) ? (y - 1) >> 1 : y >> 1;
    return far < (d->height + 1) >> 1 && far > near ? far : near;
}

static inline int clip_rgb(int v) {
    return (v & ~16383) == 0 ? v >> 6 : v < 0 ? 0 : 255;
}

static inline void yuv_to_rgb(int y, int u, int v, uint8_t *rgb) {
    const int luma = (y * 19077) >> 8;
    rgb[0] = (uint8_t)clip_rgb(luma + ((v * 26149) >> 8) - 14234);
    rgb[1] = (uint8_t)clip_rgb(luma - ((u * 6419) >> 8) - ((v * 13320) >> 8) + 8708);
    rgb[2] = (uint8_t)clip_rgb(luma + ((u * 33050) >> 8) - 17685);
}

/* libwebp's fancy upsampler: each chroma sample weighs 9:3:3:1 with its
 * neighbours, `near` being the chroma row closer to this luma row. */
static void upsample(const uint8_t *y, const uint8_t *nu, const uint8_t *nv, const uint8_t *fu,
                     const uint8_t *fv, uint8_t *dst, int len) {
    int tl_u = nu[0], tl_v = nv[0], l_u = fu[0], l_v = fv[0];
    yuv_to_rgb(y[0], (3 * tl_u + l_u + 2) >> 2, (3 * tl_v + l_v + 2) >> 2, dst);
    for (int x = 1; x <= (len - 1) >> 1; x++) {
        const int t_u = nu[x], t_v = nv[x], u = fu[x], v = fv[x];
        const int avg_u = tl_u + t_u + l_u + u + 8;
        const int avg_v = tl_v + t_v + l_v + v + 8;
        const int d12_u = (avg_u + 2 * (t_u + l_u)) >> 3;
        const int d12_v = (avg_v + 2 * (t_v + l_v)) >> 3;
        const int d03_u = (avg_u + 2 * (tl_u + u)) >> 3;
        const int d03_v = (avg_v + 2 * (tl_v + v)) >> 3;
        yuv_to_rgb(y[2 * x - 1], (d12_u + tl_u) >> 1, (d12_v + tl_v) >> 1, dst + (2 * x - 1) * 3);
        yuv_to_rgb(y[2 * x], (d03_u + t_u) >> 1, (d03_v + t_v) >> 1, dst + 2 * x * 3);
        tl_u = t_u;
        tl_v = t_v;
        l_u = u;
        l_v = v;
    }
    if (!(len & 1)) {
        yuv_to_rgb(y[len - 1], (3 * tl_u + l_u + 2) >> 2, (3 * tl_v + l_v + 2) >> 2,
                   dst + (len - 1) * 3);
    }
}

imgf_err_t webp_vp8_next_row(webp_vp8_t *d, uint8_t *rgb) {
    const int y = d->out_y;
    while (y >= d->y_final || chroma_needed(d, y) >= d->uv_final) {
        if (d->mb_y >= d->mb_h) return IMGF_ERR_INVALID_STATE;
        imgf_err_t err = decode_mb_row(d);
        if (err != IMGF_OK) return err;
    }
    const int base_y = (d->mb_y - 1) * 16 - CACHE_ABOVE;
    const int base_uv = (d->mb_y - 1) * 8 - CACHE_ABOVE;
    int near, far;
    if (y == 0) {
        near = far = 0;
    } else if (y & 1) {
        near = (y - 1) >> 1;
        far = (y + 1) >> 1 < (d->height + 1) >> 1 ? (y + 1) >> 1 : near;
    } else {
        near = y >> 1;
        far = near - 1;
    }
    const size_t n_off = (size_t)(near - base_uv) * d->uv_stride;
    const size_t f_off = (size_t)(far - base_uv) * d->uv_stride;
    upsample(d->cache_y + (size_t)(y - base_y) * d->y_stride, d->cache_u + n_off,
             d->cache_v + n_off, d->cache_u + f_off, d->cache_v + f_off, rgb, d->width);
    d->out_y++;
    return IMGF_OK;
}

imgf_err_t webp_vp8_open(webp_vp8_t **out, webp_source_t *src, size_t pos, size_t len,
                         uint32_t caps) {
    webp_vp8_t *d = (webp_vp8_t *)calloc(1, sizeof *d);
    *out = d;
    if (!d) return IMGF_ERR_OOM;
    d->src = src;

    uint8_t hdr[10];
    if (len < 10) return IMGF_ERR_DECODE;
    if (webp_source_read(src, pos, hdr, 10) != 10) return IMGF_ERR_TRUNCATED;
    const uint32_t bits = hdr[0] | ((uint32_t)hdr[1] << 8) | ((uint32_t)hdr[2] << 16);
    if ((bits & 1) || ((bits >> 1) & 7) > 3) return IMGF_ERR_DECODE;
    if (!((bits >> 4) & 1)) return IMGF_ERR_UNSUPPORTED;
    if (hdr[3] != 0x9d || hdr[4] != 0x01 || hdr[5] != 0x2a) return IMGF_ERR_DECODE;
    d->width = (hdr[6] | (hdr[7] << 8)) & 0x3fff;
    d->height = (hdr[8] | (hdr[9] << 8)) & 0x3fff;
    if (!d->width || !d->height) return IMGF_ERR_DECODE;
    const size_t part0_len = bits >> 5;
    if (part0_len > len - 10) return IMGF_ERR_DECODE;

    d->mb_w = (d->width + 15) >> 4;
    d->mb_h = (d->height + 15) >> 4;
    d->y_stride = d->mb_w * 16;
    d->uv_stride = d->mb_w * 8;
    if (!webp_cursor_init(&d->br.cur, src, pos + 10, part0_len, PART0_WINDOW, caps)) {
        return IMGF_ERR_OOM;
    }
    bd_start(&d->br);
    imgf_err_t err = parse_header(d, pos + 10 + part0_len, pos + len, caps);
    if (err != IMGF_OK) return err;
    for (int p = 0; p < d->num_parts; p++) bd_start(&d->parts[p]);

    const size_t mb_w = (size_t)d->mb_w;
    d->intra_t = (uint8_t *)imgf_alloc(4 * mb_w, caps);
    d->nz_t = (uint8_t *)imgf_alloc(mb_w, caps);
    d->nz_dc_t = (uint8_t *)imgf_alloc(mb_w, caps);
    d->top = (uint8_t *)imgf_alloc(32 * mb_w, caps);
    d->finfo = (finfo_t *)imgf_alloc(mb_w * sizeof *d->finfo, caps);
    d->work = (uint8_t *)imgf_alloc(WORK_SIZE, caps);
    d->cache_y = (uint8_t *)imgf_alloc((size_t)(CACHE_ABOVE + 16) * d->y_stride, caps);
    d->cache_u = (uint8_t *)imgf_alloc((size_t)(CACHE_ABOVE + 8) * d->uv_stride, caps);
    d->cache_v = (uint8_t *)imgf_alloc((size_t)(CACHE_ABOVE + 8) * d->uv_stride, caps);
    if (!d->intra_t || !d->nz_t || !d->nz_dc_t || !d->top || !d->finfo || !d->work ||
        !d->cache_y || !d->cache_u || !d->cache_v) {
        return IMGF_ERR_OOM;
    }
    memset(d->intra_t, B_DC_PRED, 4 * mb_w);
    memset(d->nz_t, 0, mb_w);
    memset(d->nz_dc_t, 0, mb_w);
    memset(d->work, 0, WORK_SIZE);
    return IMGF_OK;
}

int webp_vp8_width(const webp_vp8_t *d) { return d->width; }
int webp_vp8_height(const webp_vp8_t *d) { return d->height; }

void webp_vp8_destroy(webp_vp8_t *d) {
    if (!d) return;
    webp_cursor_deinit(&d->br.cur);
    for (int p = 0; p < 8; p++) webp_cursor_deinit(&d->parts[p].cur);
    imgf_free(d->intra_t);
    imgf_free(d->nz_t);
    imgf_free(d->nz_dc_t);
    imgf_free(d->top);
    imgf_free(d->finfo);
    imgf_free(d->work);
    imgf_free(d->cache_y);
    imgf_free(d->cache_u);
    imgf_free(d->cache_v);
    free(d);
}
