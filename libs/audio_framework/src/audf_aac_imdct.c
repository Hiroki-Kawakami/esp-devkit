/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_aac_internal.h"
#include "audf_aac_kernels.h"

static inline int32_t zat(const int32_t *z, int m) {
    return m & 1 ? z[1023 - (m >> 1)] : z[m >> 1];
}

static void eight_short(aac_dec_t *d, aac_chan_t *c, const int16_t *prev_win, const int16_t *win) {
    int32_t *buf = d->scratch;
    int32_t *tmp = d->scratch + 2 * AAC_FRAME_LEN;
    memset(buf, 0, 2 * AAC_FRAME_LEN * sizeof(int32_t));
    for (int w = 0; w < 8; w++) {
        int32_t *z = c->spec + 128 * w;
        aac_k_dct4_128(z, tmp);
        const int16_t *rise = w ? win : prev_win;
        int32_t *dst = buf + 448 + 128 * w;
        for (int n = 0; n < 64; n++) dst[n] += aac_mul15(z[64 + n], rise[n]);
        for (int n = 0; n < 64; n++) dst[64 + n] -= aac_mul15(z[127 - n], rise[64 + n]);
        for (int n = 0; n < 64; n++) dst[128 + n] -= aac_mul15(z[63 - n], win[127 - n]);
        for (int n = 0; n < 64; n++) dst[192 + n] -= aac_mul15(z[n], win[63 - n]);
    }
    /* c->time may be buf + AAC_FRAME_LEN. */
    for (int n = 0; n < AAC_FRAME_LEN; n++) {
        const int32_t t = c->overlap[n] + buf[n];
        c->overlap[n] = buf[AAC_FRAME_LEN + n];
        c->time[n] = t;
    }
}

void aac_filterbank(aac_dec_t *d, aac_chan_t *c) {
    const aac_ics_t *ics = &c->ics;
    const int16_t *long_win = ics->window_shape ? aac_win_kbd_long : aac_win_sine_long;
    const int16_t *short_win = ics->window_shape ? aac_win_kbd_short : aac_win_sine_short;
    const int16_t *prev_long = c->prev_shape ? aac_win_kbd_long : aac_win_sine_long;
    const int16_t *prev_short = c->prev_shape ? aac_win_kbd_short : aac_win_sine_short;
    c->prev_shape = ics->window_shape;

    if (ics->window_sequence == AAC_EIGHT_SHORT) {
        eight_short(d, c, prev_short, short_win);
        return;
    }

    const int32_t *z = c->spec;
    int32_t *time = c->time;
    int32_t *overlap = c->overlap;
    aac_k_dct4_1024(c->spec, d->scratch);
    if (ics->window_sequence != AAC_LONG_STOP) {
        for (int n = 0; n < 512; n += 2) {
            time[n] = overlap[n] + aac_mul15(z[256 + n / 2], prev_long[n]);
            time[n + 1] = overlap[n + 1] + aac_mul15(z[767 - n / 2], prev_long[n + 1]);
        }
        for (int n = 0; n < 512; n += 2) {
            time[512 + n] = overlap[512 + n] - aac_mul15(z[512 + n / 2], prev_long[512 + n]);
            time[513 + n] = overlap[513 + n] - aac_mul15(z[511 - n / 2], prev_long[513 + n]);
        }
    } else {
        for (int n = 0; n < 448; n++) time[n] = overlap[n];
        for (int n = 448; n < 512; n++) time[n] = overlap[n] + aac_mul15(zat(z, 512 + n), prev_short[n - 448]);
        for (int n = 512; n < 576; n++) time[n] = overlap[n] - aac_mul15(zat(z, 1535 - n), prev_short[n - 448]);
        for (int n = 576; n < 1024; n++) time[n] = overlap[n] - zat(z, 1535 - n);
    }
    if (ics->window_sequence != AAC_LONG_START) {
        for (int n = 0; n < 512; n += 2) {
            overlap[n] = -aac_mul15(z[768 + n / 2], long_win[1023 - n]);
            overlap[n + 1] = -aac_mul15(z[255 - n / 2], long_win[1022 - n]);
        }
        for (int n = 0; n < 512; n += 2) {
            overlap[512 + n] = -aac_mul15(z[n / 2], long_win[511 - n]);
            overlap[513 + n] = -aac_mul15(z[1023 - n / 2], long_win[510 - n]);
        }
    } else {
        for (int n = 0; n < 448; n++) overlap[n] = -zat(z, 511 - n);
        for (int n = 448; n < 512; n++) overlap[n] = -aac_mul15(zat(z, 511 - n), short_win[575 - n]);
        for (int n = 512; n < 576; n++) overlap[n] = -aac_mul15(zat(z, n - 512), short_win[575 - n]);
        for (int n = 576; n < 1024; n++) overlap[n] = 0;
    }
}
