/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_mp3_internal.h"

static int32_t dequant(unsigned code, const mp3_l2_class_t *c, uint32_t inv, unsigned scf) {
    const int32_t num = (int32_t)(2 * code + 1) - (int32_t)c->steps;
    const int32_t f = (int32_t)(((int64_t)num * inv) >> 2);
    return (int32_t)(((int64_t)f * mp3_l2_scale_q29[scf]) >> (59 - MP3_FRAC));
}

esp_err_t mp3_l2_decode(mp3_dec_t *d, int16_t *pcm) {
    const mp3_frame_t *f = &d->f;
    mp3_bits_t b = { d->frame, f->crc ? 48 : 32, f->h.bytes * 8u };
    const int nch = f->h.channels;

    const mp3_l2_alloc_t *rows;
    int sblimit;
    if (f->version) {
        rows = mp3_l2_alloc_lsf;
        sblimit = 30;
    } else {
        const unsigned kbps = nch == 2 ? f->kbps / 2u : f->kbps;
        if (kbps < 56) {
            rows = mp3_l2_alloc_low;
            sblimit = f->rate_index == 2 ? 12 : 8;
        } else {
            rows = mp3_l2_alloc_high;
            sblimit = kbps >= 96 && f->rate_index != 1 ? 30 : 27;
        }
    }
    int bound = f->mode == MP3_JOINT ? 4 + 4 * f->mode_ext : sblimit;
    if (bound > sblimit) bound = sblimit;

    uint8_t alloc[2][32] = { { 0 } };
    uint8_t scfsi[2][32];
    uint8_t scf[2][32][3];
    const mp3_l2_alloc_t *row = rows;
    int row_end = row->subbands;
    for (int sb = 0; sb < sblimit; sb++) {
        if (sb == row_end) row_end += (++row)->subbands;
        for (int ch = 0; ch < (sb < bound ? nch : 1); ch++) {
            const unsigned v = mp3_bits_get(&b, row->nbal);
            alloc[ch][sb] = v ? row->classes[v - 1] + 1 : 0;
        }
        if (sb >= bound) alloc[1][sb] = alloc[0][sb];
    }
    for (int sb = 0; sb < sblimit; sb++) {
        for (int ch = 0; ch < nch; ch++) {
            if (alloc[ch][sb]) scfsi[ch][sb] = (uint8_t)mp3_bits_get(&b, 2);
        }
    }
    for (int sb = 0; sb < sblimit; sb++) {
        for (int ch = 0; ch < nch; ch++) {
            if (!alloc[ch][sb]) continue;
            uint8_t *s = scf[ch][sb];
            s[0] = (uint8_t)mp3_bits_get(&b, 6);
            switch (scfsi[ch][sb]) {
            case 0:
                s[1] = (uint8_t)mp3_bits_get(&b, 6);
                s[2] = (uint8_t)mp3_bits_get(&b, 6);
                break;
            case 1:
                s[1] = s[0];
                s[2] = (uint8_t)mp3_bits_get(&b, 6);
                break;
            case 2:
                s[1] = s[2] = s[0];
                break;
            default:
                s[1] = s[2] = (uint8_t)mp3_bits_get(&b, 6);
                break;
            }
        }
    }

    const int out_frame = 32 * d->base.channels;
    for (int gr = 0; gr < 12; gr++) {
        const int part = gr >> 2;
        for (int ch = 0; ch < nch; ch++) {
            for (int s = 0; s < 3; s++) memset(&d->sb[ch][s][sblimit], 0, (32 - sblimit) * sizeof(int32_t));
        }
        for (int sb = 0; sb < sblimit; sb++) {
            unsigned codes[2][3];
            const int coded = sb < bound ? nch : 1;
            for (int ch = 0; ch < coded; ch++) {
                if (!alloc[ch][sb]) continue;
                const mp3_l2_class_t *c = &mp3_l2_classes[alloc[ch][sb] - 1];
                if (c->grouped) {
                    unsigned v = mp3_bits_get(&b, c->bits);
                    for (int s = 0; s < 3; s++) {
                        const unsigned q = s < 2 ? v % c->steps : v;
                        codes[ch][s] = q < c->steps ? q : c->steps - 1u;
                        v /= c->steps;
                    }
                } else {
                    for (int s = 0; s < 3; s++) codes[ch][s] = mp3_bits_get(&b, c->bits);
                }
            }
            if (coded == 1 && nch == 2) memcpy(codes[1], codes[0], sizeof(codes[0]));
            for (int ch = 0; ch < nch; ch++) {
                const unsigned a = alloc[ch][sb];
                for (int s = 0; s < 3; s++) {
                    d->sb[ch][s][sb] = a ? dequant(codes[ch][s], &mp3_l2_classes[a - 1],
                                                   mp3_l2_inv_steps_q32[a - 1], scf[ch][sb][part])
                                         : 0;
                }
            }
        }
        mp3_emit(d, 3, pcm + gr * 3 * out_frame);
    }
    return ESP_OK;
}
