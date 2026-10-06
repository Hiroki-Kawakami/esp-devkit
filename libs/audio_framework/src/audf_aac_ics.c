/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <string.h>

#include "audf_aac_internal.h"
#include "audf_aac_kernels.h"

#define SPEC_LIMIT (1 << 29)
#define SPEC_SCALE 16

esp_err_t aac_ics_info(aac_dec_t *d, aac_bits_t *b, aac_ics_t *ics) {
    aac_bits_bit(b);
    ics->window_sequence = aac_bits_get(b, 2);
    ics->window_shape = aac_bits_bit(b);
    if (ics->window_sequence == AAC_EIGHT_SHORT) {
        ics->max_sfb = aac_bits_get(b, 4);
        const uint32_t grouping = aac_bits_get(b, 7);
        ics->num_windows = 8;
        ics->num_groups = 1;
        ics->group_len[0] = 1;
        for (int i = 6; i >= 0; i--) {
            if (grouping >> i & 1) {
                ics->group_len[ics->num_groups - 1]++;
            } else {
                ics->group_len[ics->num_groups++] = 1;
            }
        }
        ics->num_swb = aac_num_swb_short[d->sf_index];
        ics->swb_offset = aac_swb_offset_short[d->sf_index];
    } else {
        ics->max_sfb = aac_bits_get(b, 6);
        if (aac_bits_bit(b)) return ESP_ERR_NOT_SUPPORTED;
        ics->num_windows = 1;
        ics->num_groups = 1;
        ics->group_len[0] = 1;
        ics->num_swb = aac_num_swb_long[d->sf_index];
        ics->swb_offset = aac_swb_offset_long[d->sf_index];
    }
    return ics->max_sfb <= ics->num_swb ? ESP_OK : ESP_ERR_INVALID_SIZE;
}

static esp_err_t section_data(aac_bits_t *b, aac_ics_t *ics) {
    const unsigned bits = ics->window_sequence == AAC_EIGHT_SHORT ? 3 : 5;
    const uint32_t esc = (1u << bits) - 1;
    for (int g = 0; g < ics->num_groups; g++) {
        int k = 0;
        while (k < ics->max_sfb) {
            const uint8_t cb = aac_bits_get(b, 4);
            if (cb == 12) return ESP_ERR_INVALID_RESPONSE;
            int len = 0;
            uint32_t incr;
            do {
                incr = aac_bits_get(b, bits);
                len += incr;
                if (aac_bits_overrun(b)) return ESP_ERR_INVALID_SIZE;
            } while (incr == esc);
            if (k + len > ics->max_sfb) return ESP_ERR_INVALID_SIZE;
            memset(&ics->cb[g][k], cb, len);
            k += len;
        }
    }
    return ESP_OK;
}

static esp_err_t scale_factor_data(aac_bits_t *b, aac_ics_t *ics) {
    int sf = ics->global_gain;
    int noise = ics->global_gain - 90;
    int is = 0;
    bool noise_pcm = true;
    for (int g = 0; g < ics->num_groups; g++) {
        for (int sfb = 0; sfb < ics->max_sfb; sfb++) {
            switch (ics->cb[g][sfb]) {
            case AAC_ZERO_HCB:
                ics->sf[g][sfb] = 0;
                break;
            case AAC_INTENSITY_HCB:
            case AAC_INTENSITY_HCB2:
                is += aac_huff(b, AAC_HCB_SF);
                if (is < -155 || is > 100) return ESP_ERR_INVALID_RESPONSE;
                ics->sf[g][sfb] = is;
                break;
            case AAC_NOISE_HCB:
                if (noise_pcm) {
                    noise_pcm = false;
                    noise += (int)aac_bits_get(b, 9) - 256;
                } else {
                    noise += aac_huff(b, AAC_HCB_SF);
                }
                if (noise < -100 || noise > 155) return ESP_ERR_INVALID_RESPONSE;
                ics->sf[g][sfb] = noise;
                break;
            default:
                sf += aac_huff(b, AAC_HCB_SF);
                if (sf < 0 || sf > 255) return ESP_ERR_INVALID_RESPONSE;
                ics->sf[g][sfb] = sf;
                break;
            }
        }
    }
    return ESP_OK;
}

static esp_err_t pulse_data(aac_bits_t *b, aac_ics_t *ics) {
    ics->pulse_n = aac_bits_get(b, 2) + 1;
    ics->pulse_start = aac_bits_get(b, 6);
    if (ics->pulse_start >= ics->num_swb) return ESP_ERR_INVALID_RESPONSE;
    for (int i = 0; i < ics->pulse_n; i++) {
        ics->pulse_offset[i] = aac_bits_get(b, 5);
        ics->pulse_amp[i] = aac_bits_get(b, 4);
    }
    return ESP_OK;
}

static esp_err_t tns_data(aac_bits_t *b, aac_ics_t *ics) {
    const bool is8 = ics->window_sequence == AAC_EIGHT_SHORT;
    const int max_order = is8 ? 7 : 12;
    for (int w = 0; w < ics->num_windows; w++) {
        aac_tns_win_t *t = &ics->tns_win[w];
        t->n_filt = aac_bits_get(b, is8 ? 1 : 2);
        if (t->n_filt) t->coef_res = aac_bits_bit(b);
        for (int f = 0; f < t->n_filt; f++) {
            t->length[f] = aac_bits_get(b, is8 ? 4 : 6);
            t->order[f] = aac_bits_get(b, is8 ? 3 : 5);
            if (t->order[f] > max_order) return ESP_ERR_INVALID_RESPONSE;
            if (!t->order[f]) continue;
            t->direction[f] = aac_bits_bit(b);
            t->compress[f] = aac_bits_bit(b);
            const unsigned nbits = 3 + t->coef_res - t->compress[f];
            for (int i = 0; i < t->order[f]; i++) t->coef[f][i] = aac_bits_get(b, nbits);
        }
    }
    return ESP_OK;
}

static int escape(aac_bits_t *b) {
    int n = 4;
    while (aac_bits_bit(b)) {
        if (++n > 12) return -1;
    }
    return (1 << n) + (int)aac_bits_get(b, n);
}

static esp_err_t decode_band(aac_bits_t *b, int cb, int32_t *dst, int width) {
    const int table = AAC_HCB1 + cb - 1;
    switch (cb) {
    case 1:
    case 2:
        for (int i = 0; i < width; i += 4) {
            const int v = aac_huff(b, table);
            dst[i] = v / 27 - 1;
            dst[i + 1] = v / 9 % 3 - 1;
            dst[i + 2] = v / 3 % 3 - 1;
            dst[i + 3] = v % 3 - 1;
        }
        break;
    case 3:
    case 4:
        for (int i = 0; i < width; i += 4) {
            const int v = aac_huff(b, table);
            int32_t q[4] = { v / 27, v / 9 % 3, v / 3 % 3, v % 3 };
            for (int j = 0; j < 4; j++) {
                if (q[j] && aac_bits_bit(b)) q[j] = -q[j];
                dst[i + j] = q[j];
            }
        }
        break;
    case 5:
    case 6:
        for (int i = 0; i < width; i += 2) {
            const int v = aac_huff(b, table);
            dst[i] = v / 9 - 4;
            dst[i + 1] = v % 9 - 4;
        }
        break;
    default: {
        const int mod = cb <= 8 ? 8 : cb <= 10 ? 13 : 17;
        for (int i = 0; i < width; i += 2) {
            const int v = aac_huff(b, table);
            int32_t y = v / mod, z = v % mod;
            const bool ny = y && aac_bits_bit(b);
            const bool nz = z && aac_bits_bit(b);
            if (cb == AAC_ESC_HCB) {
                if (y == 16 && (y = escape(b)) < 0) return ESP_ERR_INVALID_RESPONSE;
                if (z == 16 && (z = escape(b)) < 0) return ESP_ERR_INVALID_RESPONSE;
            }
            dst[i] = ny ? -y : y;
            dst[i + 1] = nz ? -z : z;
        }
        break;
    }
    }
    return aac_bits_overrun(b) ? ESP_ERR_INVALID_SIZE : ESP_OK;
}

static esp_err_t spectral_data(aac_bits_t *b, aac_chan_t *c) {
    aac_ics_t *ics = &c->ics;
    const uint16_t *swb = ics->swb_offset;
    const int stride = ics->num_windows == 8 ? 128 : 1024;
    memset(c->spec, 0, AAC_FRAME_LEN * sizeof(int32_t));
    int win = 0;
    for (int g = 0; g < ics->num_groups; g++) {
        for (int sfb = 0; sfb < ics->max_sfb; sfb++) {
            const int cb = ics->cb[g][sfb];
            if (cb == AAC_ZERO_HCB || cb >= AAC_NOISE_HCB) continue;
            const int width = swb[sfb + 1] - swb[sfb];
            for (int w = 0; w < ics->group_len[g]; w++) {
                esp_err_t err = decode_band(b, cb, c->spec + (win + w) * stride + swb[sfb], width);
                if (err != ESP_OK) return err;
            }
        }
        win += ics->group_len[g];
    }
    return ESP_OK;
}

esp_err_t aac_ics_parse(aac_dec_t *d, aac_bits_t *b, aac_chan_t *c, bool common_window) {
    aac_ics_t *ics = &c->ics;
    ics->global_gain = aac_bits_get(b, 8);
    esp_err_t err;
    if (!common_window && (err = aac_ics_info(d, b, ics)) != ESP_OK) return err;
    if ((err = section_data(b, ics)) != ESP_OK) return err;
    if ((err = scale_factor_data(b, ics)) != ESP_OK) return err;
    ics->pulse = aac_bits_bit(b);
    if (ics->pulse) {
        if (ics->window_sequence == AAC_EIGHT_SHORT) return ESP_ERR_INVALID_RESPONSE;
        if ((err = pulse_data(b, ics)) != ESP_OK) return err;
    }
    ics->tns = aac_bits_bit(b);
    if (ics->tns && (err = tns_data(b, ics)) != ESP_OK) return err;
    if (aac_bits_bit(b)) return ESP_ERR_NOT_SUPPORTED;
    if (aac_bits_overrun(b)) return ESP_ERR_INVALID_SIZE;
    return spectral_data(b, c);
}

static int32_t pow43_q13(int32_t q) {
    if (q < 16) return aac_pow43_q13[q];
    const uint64_t target = (uint64_t)q << 48;
    uint32_t lo = 0, hi = 1u << 21;
    while (lo < hi) {
        const uint32_t mid = (lo + hi + 1) >> 1;
        if ((uint64_t)mid * mid * mid <= target) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    return (int32_t)(((uint64_t)q * lo) >> 3);
}

static inline int32_t clamp_spec(int64_t v) {
    if (v > SPEC_LIMIT) return SPEC_LIMIT;
    if (v < -SPEC_LIMIT) return -SPEC_LIMIT;
    return (int32_t)v;
}

static int32_t shift_spec(int64_t v, int left) {
    if (left >= 0) {
        if (left > 30) return v ? (v > 0 ? SPEC_LIMIT : -SPEC_LIMIT) : 0;
        return clamp_spec(v * ((int64_t)1 << left));
    }
    if (left < -62) return 0;
    return clamp_spec(v >> -left);
}

static const int16_t s_pow2_quarter_q14[4] = { 16384, 19484, 23170, 27554 };
static const int32_t s_pow2_neg_quarter_q16[4] = { 0, 55109, 46341, 38968 };
static const float s_pow2_quarter[4] = { 1.0f, 1.18920712f, 1.41421356f, 1.68179283f };

static void dequant_band(int32_t *x, int width, int sf) {
    const int e = sf - 100 + SPEC_SCALE;
    const int shift = (e >> 2) - 27;
    const int64_t frac = s_pow2_quarter_q14[e & 3];
    for (int i = 0; i < width; i++) {
        const int32_t q = x[i];
        if (!q) continue;
        const int32_t m = shift_spec((int64_t)pow43_q13(q < 0 ? -q : q) * frac, shift);
        x[i] = q < 0 ? -m : m;
    }
}

static uint32_t lcg(uint32_t *seed) {
    *seed = *seed * 1664525u + 1013904223u;
    return *seed;
}

static void noise_band(int32_t *x, int width, int nrg, uint32_t *seed) {
    int64_t energy = 0;
    for (int i = 0; i < width; i++) {
        x[i] = (int32_t)lcg(seed) >> 16;
        energy += (int64_t)x[i] * x[i];
    }
    if (!energy) return;
    const int e = nrg + SPEC_SCALE;
    const float scale = s_pow2_quarter[e & 3] * ldexpf(1.0f, e >> 2) / sqrtf((float)energy);
    for (int i = 0; i < width; i++) x[i] = clamp_spec((int64_t)((float)x[i] * scale));
}

static void intensity_band(const int32_t *l, int32_t *r, int width, int pos, int sign) {
    const int a = pos >> 2;
    const int32_t frac = s_pow2_neg_quarter_q16[pos & 3];
    for (int i = 0; i < width; i++) {
        int32_t v = frac ? (int32_t)(((int64_t)l[i] * frac) >> 16) : l[i];
        v = a >= 0 ? (a > 31 ? 0 : v >> a) : shift_spec(v, -a);
        r[i] = sign < 0 ? -v : v;
    }
}

static void apply_tns(aac_dec_t *d, aac_chan_t *c) {
    aac_ics_t *ics = &c->ics;
    const bool is8 = ics->num_windows == 8;
    const int tns_max = is8 ? aac_tns_max_bands_short[d->sf_index] : aac_tns_max_bands_long[d->sf_index];
    const int mmm = tns_max < ics->max_sfb ? tns_max : ics->max_sfb;
    const uint16_t *swb = ics->swb_offset;
    float *y = (float *)d->scratch;
    for (int w = 0; w < ics->num_windows; w++) {
        const aac_tns_win_t *t = &ics->tns_win[w];
        int32_t *spec = c->spec + w * 128;
        int bottom = ics->num_swb;
        for (int f = 0; f < t->n_filt; f++) {
            const int top = bottom;
            bottom = top - t->length[f] > 0 ? top - t->length[f] : 0;
            const int order = t->order[f];
            if (!order) continue;
            const float *map = aac_tns_coef[t->coef_res * 2 + t->compress[f]];
            double a[13] = { 1.0 };
            for (int m = 1; m <= order; m++) {
                const double k = map[t->coef[f][m - 1]];
                double tmp[13];
                for (int i = 1; i < m; i++) tmp[i] = a[i] + k * a[m - i];
                for (int i = 1; i < m; i++) a[i] = tmp[i];
                a[m] = k;
            }
            float lpc[13];
            for (int i = 1; i <= order; i++) lpc[i] = (float)a[i];
            const int start = swb[bottom < mmm ? bottom : mmm];
            const int end = swb[top < mmm ? top : mmm];
            const int size = end - start;
            if (size <= 0) continue;
            const int inc = t->direction[f] ? -1 : 1;
            int32_t *x = spec + (t->direction[f] ? end - 1 : start);
            for (int m = 0; m < size; m++) {
                float acc = (float)x[m * inc];
                const int n = m < order ? m : order;
                for (int i = 1; i <= n; i++) acc -= lpc[i] * y[m - i];
                y[m] = acc;
                x[m * inc] = clamp_spec((int64_t)lrintf(acc));
            }
        }
    }
}

static void channel_spectrum(aac_dec_t *d, aac_chan_t *c, int ch) {
    aac_ics_t *ics = &c->ics;
    const uint16_t *swb = ics->swb_offset;
    const bool is8 = ics->num_windows == 8;
    const int stride = is8 ? 128 : 1024;
    if (ics->pulse) {
        int k = swb[ics->pulse_start];
        for (int i = 0; i < ics->pulse_n; i++) {
            k += ics->pulse_offset[i];
            if (k >= AAC_FRAME_LEN) break;
            c->spec[k] += c->spec[k] > 0 ? ics->pulse_amp[i] : -ics->pulse_amp[i];
        }
    }
    const aac_ics_t *left = &d->ch[0].ics;
    int win = 0;
    for (int g = 0; g < ics->num_groups; g++) {
        for (int sfb = 0; sfb < ics->max_sfb; sfb++) {
            const int cb = ics->cb[g][sfb];
            const int width = swb[sfb + 1] - swb[sfb];
            if (cb == AAC_NOISE_HCB) {
                const bool correlated = ch == 1 && d->ms_used[g][sfb] && left->cb[g][sfb] == AAC_NOISE_HCB;
                uint32_t seed = correlated ? d->noise_seeds[g][sfb] : d->noise_seed;
                if (ch == 0) d->noise_seeds[g][sfb] = d->noise_seed;
                for (int w = 0; w < ics->group_len[g]; w++) {
                    noise_band(c->spec + (win + w) * stride + swb[sfb], width, ics->sf[g][sfb], &seed);
                }
                if (!correlated) d->noise_seed = seed;
            } else if (cb != AAC_ZERO_HCB && cb < AAC_NOISE_HCB) {
                for (int w = 0; w < ics->group_len[g]; w++) {
                    dequant_band(c->spec + (win + w) * stride + swb[sfb], width, ics->sf[g][sfb]);
                }
            }
        }
        win += ics->group_len[g];
    }
}

void aac_spectrum(aac_dec_t *d, int channels) {
    for (int ch = 0; ch < channels; ch++) channel_spectrum(d, &d->ch[ch], ch);
    if (channels == 2 && d->common_window) {
        const aac_ics_t *ics = &d->ch[0].ics;
        const aac_ics_t *rics = &d->ch[1].ics;
        const uint16_t *swb = ics->swb_offset;
        const int stride = ics->num_windows == 8 ? 128 : 1024;
        int win = 0;
        for (int g = 0; g < ics->num_groups; g++) {
            for (int sfb = 0; sfb < ics->max_sfb; sfb++) {
                const int width = swb[sfb + 1] - swb[sfb];
                const int rcb = rics->cb[g][sfb];
                for (int w = 0; w < ics->group_len[g]; w++) {
                    int32_t *l = d->ch[0].spec + (win + w) * stride + swb[sfb];
                    int32_t *r = d->ch[1].spec + (win + w) * stride + swb[sfb];
                    if (rcb == AAC_INTENSITY_HCB || rcb == AAC_INTENSITY_HCB2) {
                        int sign = rcb == AAC_INTENSITY_HCB ? 1 : -1;
                        if (d->ms_used[g][sfb]) sign = -sign;
                        intensity_band(l, r, width, rics->sf[g][sfb], sign);
                    } else if (d->ms_used[g][sfb] && ics->cb[g][sfb] < AAC_NOISE_HCB && rcb < AAC_NOISE_HCB) {
                        for (int i = 0; i < width; i++) {
                            const int32_t m = l[i], s = r[i];
                            l[i] = clamp_spec((int64_t)m + s);
                            r[i] = clamp_spec((int64_t)m - s);
                        }
                    }
                }
            }
            win += ics->group_len[g];
        }
    }
    for (int ch = 0; ch < channels; ch++) {
        if (d->ch[ch].ics.tns) apply_tns(d, &d->ch[ch]);
    }
}
