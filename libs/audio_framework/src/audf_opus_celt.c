/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_opus_internal.h"

#include <math.h>
#include <string.h>

#include "audf_alloc.h"

#define BITRES 3
#define FINE_OFFSET 21
#define MAX_FINE_BITS 8
#define ALLOC_STEPS 6
#define SPREAD_NORMAL 2
#define COMB_MIN_PERIOD 15
#define PREEMPH_Q15 27853
#define DB(x) ((int32_t)((x) * 1024))

esp_err_t celt_init(celt_dec_t *st, int channels, uint32_t caps) {
    memset(st, 0, sizeof(*st));
    st->channels = (uint8_t)channels;
    for (int c = 0; c < channels; c++) {
        st->mem[c] = audf_calloc(CELT_BUF + CELT_OVERLAP, sizeof(int32_t), caps);
        if (!st->mem[c]) return ESP_ERR_NO_MEM;
    }
    st->freq = audf_malloc(2 * CELT_MAX_FRAME * sizeof(int32_t), caps);
    st->x = audf_malloc(2 * CELT_MAX_FRAME * sizeof(int32_t), caps);
    st->norm = audf_malloc(2 * 8 * 100 * sizeof(int32_t), caps);
    st->scratch = audf_malloc(2 * 176 * sizeof(int32_t), caps);
    st->iy = audf_malloc(CELT_MAX_PVQ_N * sizeof(int), caps);
    if (!st->freq || !st->x || !st->norm || !st->scratch || !st->iy) return ESP_ERR_NO_MEM;
    esp_err_t err = celt_pvq_init(&st->pvq, caps);
    if (err == ESP_OK) err = celt_mdct_init(&st->mdct, caps);
    if (err != ESP_OK) return err;
    celt_reset(st);
    return ESP_OK;
}

void celt_free(celt_dec_t *st) {
    for (int c = 0; c < 2; c++) audf_free(st->mem[c]);
    audf_free(st->freq);
    audf_free(st->x);
    audf_free(st->norm);
    audf_free(st->scratch);
    audf_free(st->iy);
    audf_free(st->pvq.v);
    celt_mdct_free(&st->mdct);
}

void celt_reset(celt_dec_t *st) {
    for (int c = 0; c < st->channels; c++) {
        memset(st->mem[c], 0, (CELT_BUF + CELT_OVERLAP) * sizeof(int32_t));
    }
    st->stream_channels = st->channels;
    st->start = 0;
    st->end = CELT_BANDS;
    st->rng = 0;
    st->loss_count = 0;
    st->pf_period = st->pf_period_old = 0;
    st->pf_gain = st->pf_gain_old = 0;
    st->pf_tapset = st->pf_tapset_old = 0;
    st->preemph_mem[0] = st->preemph_mem[1] = 0;
    for (int i = 0; i < 2 * CELT_BANDS; i++) {
        st->old_e[i] = 0;
        st->old_loge[i] = st->old_loge2[i] = -DB(28);
        st->background_loge[i] = 0;
    }
}

static int laplace_decode(opus_rc_t *rc, uint32_t fs, int decay) {
    int val = 0;
    uint32_t fl = 0;
    const uint32_t fm = opus_rc_decode_bin(rc, 15);
    if (fm >= fs) {
        val++;
        fl = fs;
        fs = ((32768 - 32 - fs) * (uint32_t)(16384 - decay) >> 15) + 1;
        while (fs > 1 && fm >= fl + 2 * fs) {
            fs *= 2;
            fl += fs;
            fs = ((fs - 2) * (uint32_t)decay >> 15) + 1;
            val++;
        }
        if (fs <= 1) {
            const uint32_t di = (fm - fl) >> 1;
            val += (int)di;
            fl += 2 * di;
        }
        if (fm < fl + fs) {
            val = -val;
        } else {
            fl += fs;
        }
    }
    opus_rc_update(rc, fl, fl + fs < 32768 ? fl + fs : 32768, 32768);
    return val;
}

static void decode_coarse_energy(celt_dec_t *st, opus_rc_t *rc, int intra, int c_count, int lm) {
    const uint8_t *prob = celt_e_prob[lm][intra];
    int32_t prev[2] = { 0, 0 };
    const int32_t coef = intra ? 0 : celt_pred_coef[lm];
    const int32_t beta = intra ? 4915 : celt_beta_coef[lm];
    const int32_t budget = (int32_t)rc->storage * 8;
    for (int i = st->start; i < st->end; i++) {
        for (int c = 0; c < c_count; c++) {
            const int32_t tell = opus_rc_tell(rc);
            int qi;
            if (budget - tell >= 15) {
                const int pi = 2 * opus_imin(i, 20);
                qi = laplace_decode(rc, (uint32_t)prob[pi] << 7, prob[pi + 1] << 6);
            } else if (budget - tell >= 2) {
                qi = opus_rc_icdf(rc, celt_small_energy_icdf, 2);
                qi = (qi >> 1) ^ -(qi & 1);
            } else if (budget - tell >= 1) {
                qi = -opus_rc_bit_logp(rc, 1);
            } else {
                qi = -1;
            }
            const int32_t q = qi * 1024;
            int32_t *e = &st->old_e[c * CELT_BANDS + i];
            if (*e < -DB(9)) *e = -DB(9);
            int32_t tmp = ((coef * *e + 128) >> 8) + prev[c] + q * 128;
            if (tmp < -DB(28) * 128) tmp = -DB(28) * 128;
            *e = (tmp + 64) >> 7;
            prev[c] = prev[c] + q * 128 - beta * ((q + 128) >> 8);
        }
    }
}

static void decode_tf(celt_dec_t *st, opus_rc_t *rc, int transient, int *tf_res, int lm) {
    uint32_t budget = rc->storage * 8;
    uint32_t tell = (uint32_t)opus_rc_tell(rc);
    int logp = transient ? 2 : 4;
    const int select_rsv = lm > 0 && tell + logp + 1 <= budget;
    budget -= select_rsv;
    int changed = 0, curr = 0;
    for (int i = st->start; i < st->end; i++) {
        if (tell + logp <= budget) {
            curr ^= opus_rc_bit_logp(rc, logp);
            tell = (uint32_t)opus_rc_tell(rc);
            changed |= curr;
        }
        tf_res[i] = curr;
        logp = transient ? 4 : 5;
    }
    int select = 0;
    if (select_rsv && celt_tf_select[lm][4 * transient + changed] !=
                          celt_tf_select[lm][4 * transient + 2 + changed]) {
        select = opus_rc_bit_logp(rc, 1);
    }
    for (int i = st->start; i < st->end; i++) {
        tf_res[i] = celt_tf_select[lm][4 * transient + 2 * select + tf_res[i]];
    }
}

static int interp_bits2pulses(int start, int end, int skip_start, const int *bits1, const int *bits2,
                              const int *thresh, const int *cap, int32_t total, int skip_rsv,
                              int intensity_rsv, int dual_stereo_rsv, int c_count, int lm,
                              opus_rc_t *rc, celt_alloc_t *a) {
    int *bits = a->pulses;
    int *ebits = a->fine;
    const int alloc_floor = c_count << BITRES;
    const int stereo = c_count > 1;
    const int log_m = lm << BITRES;
    int lo = 0, hi = 1 << ALLOC_STEPS;
    for (int i = 0; i < ALLOC_STEPS; i++) {
        const int mid = (lo + hi) >> 1;
        int32_t psum = 0;
        int done = 0;
        for (int j = end; j-- > start;) {
            const int tmp = bits1[j] + (mid * bits2[j] >> ALLOC_STEPS);
            if (tmp >= thresh[j] || done) {
                done = 1;
                psum += opus_imin(tmp, cap[j]);
            } else if (tmp >= alloc_floor) {
                psum += alloc_floor;
            }
        }
        if (psum > total) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    int32_t psum = 0;
    int done = 0;
    for (int j = end; j-- > start;) {
        int tmp = bits1[j] + (lo * bits2[j] >> ALLOC_STEPS);
        if (tmp < thresh[j] && !done) {
            tmp = tmp >= alloc_floor ? alloc_floor : 0;
        } else {
            done = 1;
        }
        tmp = opus_imin(tmp, cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }

    int coded;
    for (coded = end;; coded--) {
        const int j = coded - 1;
        if (j <= skip_start) {
            total += skip_rsv;
            break;
        }
        int32_t left = total - psum;
        const int32_t percoeff = left / (celt_ebands[coded] - celt_ebands[start]);
        left -= (celt_ebands[coded] - celt_ebands[start]) * percoeff;
        const int rem = opus_imax((int)left - (celt_ebands[j] - celt_ebands[start]), 0);
        const int band_width = celt_ebands[coded] - celt_ebands[j];
        int band_bits = (int)(bits[j] + percoeff * band_width + rem);
        if (band_bits >= opus_imax(thresh[j], alloc_floor + (1 << BITRES))) {
            if (opus_rc_bit_logp(rc, 1)) break;
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if (intensity_rsv > 0) intensity_rsv = celt_log2_frac[j - start];
        psum += intensity_rsv;
        if (band_bits >= alloc_floor) {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
    }

    if (intensity_rsv > 0) {
        a->intensity = start + (int)opus_rc_uint(rc, (uint32_t)(coded + 1 - start));
    } else {
        a->intensity = 0;
    }
    if (a->intensity <= start) {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    a->dual_stereo = dual_stereo_rsv > 0 ? opus_rc_bit_logp(rc, 1) : 0;

    int32_t left = total - psum;
    const int32_t percoeff = left / (celt_ebands[coded] - celt_ebands[start]);
    left -= (celt_ebands[coded] - celt_ebands[start]) * percoeff;
    for (int j = start; j < coded; j++) bits[j] += (int)percoeff * (celt_ebands[j + 1] - celt_ebands[j]);
    for (int j = start; j < coded; j++) {
        const int tmp = (int)(left < celt_ebands[j + 1] - celt_ebands[j] ? left : celt_ebands[j + 1] - celt_ebands[j]);
        bits[j] += tmp;
        left -= tmp;
    }

    int balance = 0;
    int j;
    for (j = start; j < coded; j++) {
        const int n0 = celt_ebands[j + 1] - celt_ebands[j];
        const int n = n0 << lm;
        int excess;
        bits[j] += balance;
        if (n > 1) {
            excess = opus_imax(bits[j] - cap[j], 0);
            bits[j] -= excess;
            const int den = c_count * n + ((c_count == 2 && n > 2 && !a->dual_stereo && j < a->intensity) ? 1 : 0);
            const int nclogn = den * (celt_log_n[j] + log_m);
            int offset = (nclogn >> 1) - den * FINE_OFFSET;
            if (n == 2) offset += den << BITRES >> 2;
            if (bits[j] + offset < den * 2 << BITRES) {
                offset += nclogn >> 2;
            } else if (bits[j] + offset < den * 3 << BITRES) {
                offset += nclogn >> 3;
            }
            ebits[j] = opus_imax(0, (bits[j] + offset + (den << (BITRES - 1))) / (den << BITRES));
            if (c_count * ebits[j] > (bits[j] >> BITRES)) ebits[j] = bits[j] >> stereo >> BITRES;
            ebits[j] = opus_imin(ebits[j], MAX_FINE_BITS);
            a->priority[j] = ebits[j] * (den << BITRES) >= bits[j] + offset;
            bits[j] -= c_count * ebits[j] << BITRES;
        } else {
            excess = opus_imax(0, bits[j] - (c_count << BITRES));
            bits[j] -= excess;
            ebits[j] = 0;
            a->priority[j] = 1;
        }
        if (excess > 0) {
            const int extra_fine = opus_imin(excess >> (stereo + BITRES), MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            const int extra_bits = extra_fine * c_count << BITRES;
            a->priority[j] = extra_bits >= excess - balance;
            excess -= extra_bits;
        }
        balance = excess;
    }
    a->balance = balance;
    for (; j < end; j++) {
        ebits[j] = bits[j] >> stereo >> BITRES;
        bits[j] = 0;
        a->priority[j] = ebits[j] < 1;
    }
    return coded;
}

static void compute_allocation(celt_frame_t *fr, int start, int end, int trim, int32_t total,
                               int c_count, int lm, opus_rc_t *rc) {
    const int *offsets = fr->offsets;
    const int *cap = fr->cap;
    int *bits1 = fr->bits1, *bits2 = fr->bits2, *thresh = fr->thresh, *trim_offset = fr->trim_offset;
    celt_alloc_t *a = &fr->alloc;
    total = total > 0 ? total : 0;
    int skip_start = start;
    const int skip_rsv = total >= 1 << BITRES ? 1 << BITRES : 0;
    total -= skip_rsv;
    int intensity_rsv = 0, dual_stereo_rsv = 0;
    if (c_count == 2) {
        intensity_rsv = celt_log2_frac[end - start];
        if (intensity_rsv > total) {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = total >= 1 << BITRES ? 1 << BITRES : 0;
            total -= dual_stereo_rsv;
        }
    }
    for (int j = start; j < end; j++) {
        const int w = celt_ebands[j + 1] - celt_ebands[j];
        thresh[j] = opus_imax(c_count << BITRES, (3 * w << lm << BITRES) >> 4);
        trim_offset[j] = c_count * w * (trim - 5 - lm) * (end - j - 1) * (1 << (lm + BITRES)) >> 6;
        if (w << lm == 1) trim_offset[j] -= c_count << BITRES;
    }
    int lo = 1, hi = CELT_ALLOC_VECTORS - 1;
    do {
        int done = 0;
        int psum = 0;
        const int mid = (lo + hi) >> 1;
        for (int j = end; j-- > start;) {
            const int w = celt_ebands[j + 1] - celt_ebands[j];
            int bitsj = c_count * w * celt_alloc[mid][j] << lm >> 2;
            if (bitsj > 0) bitsj = opus_imax(0, bitsj + trim_offset[j]);
            bitsj += offsets[j];
            if (bitsj >= thresh[j] || done) {
                done = 1;
                psum += opus_imin(bitsj, cap[j]);
            } else if (bitsj >= c_count << BITRES) {
                psum += c_count << BITRES;
            }
        }
        if (psum > total) {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    } while (lo <= hi);
    hi = lo--;
    for (int j = start; j < end; j++) {
        const int w = celt_ebands[j + 1] - celt_ebands[j];
        int b1 = c_count * w * celt_alloc[lo][j] << lm >> 2;
        int b2 = hi >= CELT_ALLOC_VECTORS ? cap[j] : c_count * w * celt_alloc[hi][j] << lm >> 2;
        if (b1 > 0) b1 = opus_imax(0, b1 + trim_offset[j]);
        if (b2 > 0) b2 = opus_imax(0, b2 + trim_offset[j]);
        if (lo > 0) b1 += offsets[j];
        b2 += offsets[j];
        if (offsets[j] > 0) skip_start = j;
        bits1[j] = b1;
        bits2[j] = opus_imax(0, b2 - b1);
    }
    a->coded_bands = interp_bits2pulses(start, end, skip_start, bits1, bits2, thresh, cap, total,
                                        skip_rsv, intensity_rsv, dual_stereo_rsv, c_count, lm, rc, a);
}

static void decode_fine_energy(celt_dec_t *st, opus_rc_t *rc, const int *fine, int c_count) {
    for (int i = st->start; i < st->end; i++) {
        if (fine[i] <= 0) continue;
        for (int c = 0; c < c_count; c++) {
            const int32_t q2 = (int32_t)opus_rc_bits(rc, fine[i]);
            st->old_e[c * CELT_BANDS + i] += ((q2 * 1024 + 512) >> fine[i]) - 512;
        }
    }
}

static void decode_energy_finalise(celt_dec_t *st, opus_rc_t *rc, const int *fine,
                                   const int *priority, int bits_left, int c_count) {
    for (int prio = 0; prio < 2; prio++) {
        for (int i = st->start; i < st->end && bits_left >= c_count; i++) {
            if (fine[i] >= MAX_FINE_BITS || priority[i] != prio) continue;
            for (int c = 0; c < c_count; c++) {
                const int32_t q2 = (int32_t)opus_rc_bits(rc, 1);
                st->old_e[c * CELT_BANDS + i] += (q2 * 1024 - 512) >> (fine[i] + 1);
                bits_left--;
            }
        }
    }
}

static void denormalise(const celt_dec_t *st, const int32_t *log_e, const int32_t *x, int32_t *freq,
                        int end, int c_count, int lm, bool silence) {
    const int m = 1 << lm;
    const int n = m * 120;
    const float scale = (float)(1 << SIG_SHIFT) / (float)NORM_ONE;
    for (int c = 0; c < c_count; c++) {
        int32_t *f = freq + c * n;
        memset(f, 0, (size_t)n * sizeof(*f));
        if (silence) continue;
        for (int i = st->start; i < end; i++) {
            int32_t lg = log_e[c * CELT_BANDS + i] + (celt_e_means[i] << 6);
            if (lg > DB(32)) lg = DB(32);
            const float g = opus_exp2((float)lg / 1024.0f) * scale;
            for (int j = m * celt_ebands[i]; j < m * celt_ebands[i + 1]; j++) {
                float v = (float)x[c * n + j] * g;
                if (v > (float)SIG_SAT) v = (float)SIG_SAT;
                if (v < -(float)SIG_SAT) v = -(float)SIG_SAT;
                f[j] = (int32_t)lrintf(v);
            }
        }
    }
}

/* y = x + g * taps(y[-T]) in place, cross-fading from the old period, gain
 * and taps to the new ones over the first CELT_OVERLAP samples. The tap
 * weights are rounded to Q15 as the reference decoder rounds them, which the
 * test vectors were made with. Each opus_mulh() floors, and in this recursive
 * filter that bias builds up audibly in quiet passages, so one LSB per term
 * is added back. */
static void comb_filter(int32_t *y, int t0, int t1, int n, int32_t g0, int32_t g1, int tapset0,
                        int tapset1, bool fade) {
    if (g0 == 0 && g1 == 0) return;
    const int32_t g00 = g0 * celt_comb_gains[tapset0][0] >> 15;
    const int32_t g01 = g0 * celt_comb_gains[tapset0][1] >> 15;
    const int32_t g02 = g0 * celt_comb_gains[tapset0][2] >> 15;
    const int32_t g10 = g1 * celt_comb_gains[tapset1][0] >> 15;
    const int32_t g11 = g1 * celt_comb_gains[tapset1][1] >> 15;
    const int32_t g12 = g1 * celt_comb_gains[tapset1][2] >> 15;
    int i = 0;
    if (fade) {
        for (; i < CELT_OVERLAP && i < n; i++) {
            const int32_t f = celt_window[i] * celt_window[i] >> 15;
            const int32_t a0 = ((32767 - f) * g00 >> 15) << 16, a1 = ((32767 - f) * g01 >> 15) << 16;
            const int32_t a2 = ((32767 - f) * g02 >> 15) << 16;
            const int32_t b0 = (f * g10 >> 15) << 16, b1 = (f * g11 >> 15) << 16, b2 = (f * g12 >> 15) << 16;
            const int32_t acc = opus_mulh(y[i - t0], a0) + opus_mulh(y[i - t0 - 1] + y[i - t0 + 1], a1) +
                                opus_mulh(y[i - t0 - 2] + y[i - t0 + 2], a2) + opus_mulh(y[i - t1], b0) +
                                opus_mulh(y[i - t1 - 1] + y[i - t1 + 1], b1) +
                                opus_mulh(y[i - t1 - 2] + y[i - t1 + 2], b2);
            y[i] = celt_sat_sig((int64_t)y[i] + acc * 2 + 6);
        }
    }
    if (g1 == 0) return;
    const int32_t c0 = g10 << 16, c1 = g11 << 16, c2 = g12 << 16;
    for (; i < n; i++) {
        const int32_t acc = opus_mulh(y[i - t1], c0) + opus_mulh(y[i - t1 - 1] + y[i - t1 + 1], c1) +
                            opus_mulh(y[i - t1 - 2] + y[i - t1 + 2], c2);
        y[i] = celt_sat_sig((int64_t)y[i] + acc * 2 + 3);
    }
}

static void synthesize(celt_dec_t *st, const int32_t *log_e, int c_count, int lm, bool short_blocks,
                       int effective_end, bool silence) {
    const int cc = st->channels;
    const int n = 120 << lm;
    denormalise(st, log_e, st->x, st->freq, effective_end, c_count, lm, silence);
    if (cc == 2 && c_count == 1) {
        memcpy(st->freq + n, st->freq, (size_t)n * sizeof(int32_t));
    } else if (cc == 1 && c_count == 2) {
        for (int i = 0; i < n; i++) st->freq[i] = (int32_t)(((int64_t)st->freq[i] + st->freq[n + i]) >> 1);
    }
    for (int c = 0; c < cc; c++) {
        int32_t *mem = st->mem[c];
        memmove(mem, mem + n, (size_t)(CELT_BUF - n) * sizeof(int32_t));
        memcpy(mem + CELT_BUF - n, mem + CELT_BUF, CELT_OVERLAP * sizeof(int32_t));
        int32_t *out = mem + CELT_BUF - n;
        const int32_t *freq = st->freq + c * n;
        if (short_blocks) {
            const int blocks = 1 << lm;
            for (int b = 0; b < blocks; b++) celt_imdct(&st->mdct, freq + b, blocks, 3, out + 120 * b);
        } else {
            celt_imdct(&st->mdct, freq, 1, 3 - lm, out);
        }
    }
}

static void deemphasis(celt_dec_t *st, int n, int32_t *pcm) {
    const int cc = st->channels;
    for (int c = 0; c < cc; c++) {
        const int32_t *x = st->mem[c] + CELT_BUF - n;
        int32_t m = st->preemph_mem[c];
        for (int j = 0; j < n; j++) {
            const int32_t t = celt_sat_sig((int64_t)x[j] + m);
            m = opus_mulh(t, PREEMPH_Q15 << 16) * 2 + 1; /* the floor's bias, as in comb_filter() */
            pcm[j * cc + c] = t;
        }
        st->preemph_mem[c] = m;
    }
}

static void decode_lost(celt_dec_t *st, int lm, int32_t *pcm) {
    const int n = 120 << lm;
    const int c_count = st->channels;
    const int m = 1 << lm;
    if (st->loss_count < 5) {
        const int32_t decay = st->loss_count == 0 ? DB(1.5) : DB(0.5);
        for (int c = 0; c < c_count; c++) {
            for (int i = st->start; i < st->end; i++) st->old_e[c * CELT_BANDS + i] -= decay;
        }
    }
    uint32_t seed = st->rng;
    for (int c = 0; c < c_count; c++) {
        int32_t *x = st->x + c * n;
        memset(x, 0, (size_t)n * sizeof(*x));
        for (int i = st->start; i < st->end; i++) {
            const int off = m * celt_ebands[i];
            const int len = m * (celt_ebands[i + 1] - celt_ebands[i]);
            for (int j = 0; j < len; j++) {
                seed = celt_lcg(seed);
                x[off + j] = (int32_t)seed >> 12;
            }
            celt_renormalise(x + off, len, 1.0f);
        }
    }
    st->rng = seed;
    synthesize(st, st->loss_count >= 5 ? st->background_loge : st->old_e, c_count, lm, false, st->end,
               false);
    deemphasis(st, n, pcm);
    st->loss_count++;
}

esp_err_t celt_decode(celt_dec_t *st, opus_rc_t *rc, uint32_t len, int lm, int32_t *pcm) {
    const int n = 120 << lm;
    const int m = 1 << lm;
    const int cc = st->channels;
    const int c_count = st->stream_channels;

    if (!rc || len <= 1) {
        decode_lost(st, lm, pcm);
        return ESP_OK;
    }

    const int effective_end = st->end;
    if (c_count == 1) {
        for (int i = 0; i < CELT_BANDS; i++) {
            if (st->old_e[CELT_BANDS + i] > st->old_e[i]) st->old_e[i] = st->old_e[CELT_BANDS + i];
        }
    }

    int32_t total_bits = (int32_t)len * 8;
    int32_t tell = opus_rc_tell(rc);
    int silence;
    if (tell >= total_bits) {
        silence = 1;
    } else if (tell == 1) {
        silence = opus_rc_bit_logp(rc, 15);
    } else {
        silence = 0;
    }
    if (silence) {
        tell = (int32_t)len * 8;
        rc->nbits_total += tell - opus_rc_tell(rc);
    }

    int32_t pf_gain = 0;
    int pf_pitch = 0, pf_tapset = 0;
    if (st->start == 0 && tell + 16 <= total_bits) {
        if (opus_rc_bit_logp(rc, 1)) {
            const int octave = (int)opus_rc_uint(rc, 6);
            pf_pitch = (16 << octave) + (int)opus_rc_bits(rc, 4 + octave) - 1;
            const int qg = (int)opus_rc_bits(rc, 3);
            if (opus_rc_tell(rc) + 2 <= total_bits) pf_tapset = opus_rc_icdf(rc, celt_tapset_icdf, 2);
            pf_gain = 3072 * (qg + 1);
        }
        tell = opus_rc_tell(rc);
    }

    int transient = 0;
    if (lm > 0 && tell + 3 <= total_bits) {
        transient = opus_rc_bit_logp(rc, 3);
        tell = opus_rc_tell(rc);
    }
    const int intra = tell + 3 <= total_bits ? opus_rc_bit_logp(rc, 3) : 0;
    decode_coarse_energy(st, rc, intra, c_count, lm);

    celt_frame_t *fr = &st->frame;
    int *tf_res = fr->tf_res;
    memset(tf_res, 0, sizeof(fr->tf_res));
    decode_tf(st, rc, transient, tf_res, lm);

    tell = opus_rc_tell(rc);
    int spread = SPREAD_NORMAL;
    if (tell + 4 <= total_bits) spread = opus_rc_icdf(rc, celt_spread_icdf, 5);

    int *cap = fr->cap;
    for (int i = 0; i < CELT_BANDS; i++) {
        const int w = (celt_ebands[i + 1] - celt_ebands[i]) << lm;
        cap[i] = (celt_cache_caps[CELT_BANDS * (2 * lm + c_count - 1) + i] + 64) * c_count * w >> 2;
    }

    int *offsets = fr->offsets;
    memset(offsets, 0, sizeof(fr->offsets));
    int dynalloc_logp = 6;
    total_bits <<= BITRES;
    tell = (int32_t)opus_rc_tell_frac(rc);
    for (int i = st->start; i < st->end; i++) {
        const int width = c_count * (celt_ebands[i + 1] - celt_ebands[i]) << lm;
        const int quanta = opus_imin(width << BITRES, opus_imax(6 << BITRES, width));
        int loop_logp = dynalloc_logp;
        int boost = 0;
        while (tell + (loop_logp << BITRES) < total_bits && boost < cap[i]) {
            const int flag = opus_rc_bit_logp(rc, loop_logp);
            tell = (int32_t)opus_rc_tell_frac(rc);
            if (!flag) break;
            boost += quanta;
            total_bits -= quanta;
            loop_logp = 1;
        }
        offsets[i] = boost;
        if (boost > 0) dynalloc_logp = opus_imax(2, dynalloc_logp - 1);
    }

    const int trim = tell + (6 << BITRES) <= total_bits ? opus_rc_icdf(rc, celt_trim_icdf, 7) : 5;

    int32_t bits = (((int32_t)len * 8) << BITRES) - (int32_t)opus_rc_tell_frac(rc) - 1;
    const int anti_collapse_rsv = transient && lm >= 2 && bits >= (lm + 2) << BITRES ? 1 << BITRES : 0;
    bits -= anti_collapse_rsv;
    compute_allocation(fr, st->start, st->end, trim, bits, c_count, lm, rc);
    const celt_alloc_t *alloc = &fr->alloc;

    decode_fine_energy(st, rc, alloc->fine, c_count);

    int32_t *x = st->x;
    memset(x, 0, (size_t)c_count * n * sizeof(int32_t));
    uint8_t *collapse = fr->collapse;
    celt_bands_t *ctx = &fr->bands;
    *ctx = (celt_bands_t){
        .rc = rc,
        .pvq = &st->pvq,
        .lm = lm,
        .spread = spread,
        .intensity = alloc->intensity,
        .stereo_inversion = st->stereo_inversion,
        .seed = st->rng,
        .scratch = st->scratch,
        .lowband_scratch = st->scratch + 176,
        .iy = st->iy,
        .coded_bands = alloc->coded_bands,
    };
    celt_quant_all_bands(ctx, st->start, st->end, x, c_count == 2 ? x + n : NULL, collapse,
                         alloc->pulses, transient ? m : 0, alloc->dual_stereo, tf_res,
                         (int32_t)len * (8 << BITRES) - anti_collapse_rsv, alloc->balance, st->norm);

    int anti_collapse_on = 0;
    if (anti_collapse_rsv > 0) anti_collapse_on = (int)opus_rc_bits(rc, 1);
    decode_energy_finalise(st, rc, alloc->fine, alloc->priority, (int)len * 8 - opus_rc_tell(rc), c_count);
    if (anti_collapse_on) {
        celt_anti_collapse(x, collapse, lm, c_count, n, st->start, st->end, st->old_e, st->old_loge,
                           st->old_loge2, alloc->pulses, ctx->seed);
    }

    if (silence) {
        for (int i = 0; i < c_count * CELT_BANDS; i++) st->old_e[i] = -DB(28);
    }
    synthesize(st, st->old_e, c_count, lm, transient, effective_end, silence);

    for (int c = 0; c < cc; c++) {
        int32_t *out = st->mem[c] + CELT_BUF - n;
        if (st->pf_period < COMB_MIN_PERIOD) st->pf_period = COMB_MIN_PERIOD;
        if (st->pf_period_old < COMB_MIN_PERIOD) st->pf_period_old = COMB_MIN_PERIOD;
        comb_filter(out, st->pf_period_old, st->pf_period, 120, st->pf_gain_old, st->pf_gain,
                    st->pf_tapset_old, st->pf_tapset, true);
        if (lm != 0) {
            comb_filter(out + 120, st->pf_period, pf_pitch, n - 120, st->pf_gain, pf_gain,
                        st->pf_tapset, pf_tapset, true);
        }
    }
    st->pf_period_old = st->pf_period;
    st->pf_gain_old = st->pf_gain;
    st->pf_tapset_old = st->pf_tapset;
    st->pf_period = pf_pitch;
    st->pf_gain = pf_gain;
    st->pf_tapset = pf_tapset;
    if (lm != 0) {
        st->pf_period_old = st->pf_period;
        st->pf_gain_old = st->pf_gain;
        st->pf_tapset_old = st->pf_tapset;
    }

    if (c_count == 1) memcpy(st->old_e + CELT_BANDS, st->old_e, CELT_BANDS * sizeof(int32_t));
    if (!transient) {
        memcpy(st->old_loge2, st->old_loge, sizeof(st->old_loge));
        memcpy(st->old_loge, st->old_e, sizeof(st->old_e));
        for (int i = 0; i < 2 * CELT_BANDS; i++) {
            const int32_t b = st->background_loge[i] + m;
            st->background_loge[i] = b < st->old_e[i] ? b : st->old_e[i];
        }
    } else {
        for (int i = 0; i < 2 * CELT_BANDS; i++) {
            if (st->old_e[i] < st->old_loge[i]) st->old_loge[i] = st->old_e[i];
        }
    }
    for (int c = 0; c < 2; c++) {
        for (int i = 0; i < CELT_BANDS; i++) {
            if (i >= st->start && i < st->end) continue;
            st->old_e[c * CELT_BANDS + i] = 0;
            st->old_loge[c * CELT_BANDS + i] = st->old_loge2[c * CELT_BANDS + i] = -DB(28);
        }
    }
    st->rng = rc->rng;
    deemphasis(st, n, pcm);
    st->loss_count = 0;
    if (opus_rc_tell(rc) > 8 * (int32_t)len) return ESP_ERR_INVALID_SIZE;
    return ESP_OK;
}
