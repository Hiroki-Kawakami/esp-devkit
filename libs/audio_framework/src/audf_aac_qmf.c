/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_aac_kernels.h"
#include "audf_aac_sbr.h"

void sbr_qmf_analysis(sbr_ch_t *c, const int32_t *time, int bands, int32_t *tmp) {
    int32_t *a = tmp;
    int32_t *work = tmp + 256;
    int32_t *u = tmp + 512;
    memmove(c->x_low, c->x_low + SBR_SLOTS * 64, SBR_HF_GEN * 64 * sizeof(int32_t));
    memmove(c->ana_hi, c->ana_hi + AAC_FRAME_LEN, 288 * sizeof(int16_t));
    memmove(c->ana_lo, c->ana_lo + AAC_FRAME_LEN, 288 * sizeof(int16_t));
    for (int n = 0; n < AAC_FRAME_LEN; n++) aac_split(c->ana_hi + 288 + n, c->ana_lo + 288 + n, time[n] * (1 << 7));
    for (int g = 0; g < SBR_SLOTS; g += 4) {
        for (int l = 0; l < 4; l++) {
            aac_k_qmf_analysis(u, c->ana_hi + 32 * (g + l) + 256, c->ana_lo + 32 * (g + l) + 256, sbr_qmf_c2r);
            a[l] = u[63] * 8;
            a[4 + l] = u[62] * 8;
            for (int k = 1; k < 32; k++) {
                a[8 * k + l] = -u[k - 1] * 8;
                a[8 * k + 4 + l] = u[62 - k] * 8;
            }
        }
        aac_k_dct4_64x4(a, work);
        for (int l = 0; l < 4; l++) {
            int32_t *out = c->x_low + (SBR_HF_GEN + g + l) * 64;
            for (int k = 0; k < bands; k++) {
                out[2 * k] = a[4 * k + l];
                out[2 * k + 1] = -a[4 * (63 - k) + l];
            }
        }
    }
}

void sbr_qmf_synthesis(sbr_ch_t *c, const int32_t *const x[4], int32_t *acc, int32_t *tmp) {
    int32_t *a = tmp;
    int32_t *b = tmp + 256;
    int32_t *work = tmp + 512;
    aac_k_syn_in4(a, b, x);
    aac_k_dct4_64x4(a, work);
    aac_k_dct4_64x4(b, work);
    if (c->v_off < 512) {
        memmove(c->v_hi + SBR_V_LEN - 1152, c->v_hi + c->v_off, 1152 * sizeof(int16_t));
        memmove(c->v_lo + SBR_V_LEN - 1152, c->v_lo + c->v_off, 1152 * sizeof(int16_t));
        c->v_off = SBR_V_LEN - 1152;
    }
    aac_k_syn_v4(c->v_hi + c->v_off, c->v_lo + c->v_off, a, b);
    for (int l = 0; l < 4; l++) {
        c->v_off -= 128;
        aac_k_qmf_synthesis(acc + 64 * l, c->v_hi + c->v_off, c->v_lo + c->v_off, sbr_qmf_c);
    }
}
