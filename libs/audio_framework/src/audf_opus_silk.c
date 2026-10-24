/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_opus_internal.h"

#include <math.h>
#include <string.h>

#include "audf_alloc.h"
#include "audf_opus_silk_tables.h"

#define MAX_FRAME   320
#define MAX_SUBFR   80
#define LTP_MEM     320
#define MAX_ORDER   16

enum {
    TYPE_INACTIVE = 0,
    TYPE_UNVOICED,
    TYPE_VOICED,
};

enum {
    CODE_INDEPENDENTLY = 0,
    CODE_INDEPENDENTLY_NO_LTP_SCALING,
    CODE_CONDITIONALLY,
};

typedef struct {
    int32_t iir[6];
    int16_t fir[8];
    int16_t delay[16];
    int     input_delay;
    int     fs_in_khz;
    int32_t inv_ratio_q16;
} resampler_t;

typedef struct {
    int signal_type;
    int offset_type;
    int gains[4];
    int nlsf[MAX_ORDER + 1];
    int interp;
    int lag_index;
    int contour;
    int per_index;
    int ltp_index[4];
    int ltp_scale;
    int seed;
} indices_t;

typedef struct {
    int32_t gain_q16[4];
    int16_t a_q12[2][MAX_ORDER];
    int     pitch[4];
    int8_t  ltp_q7[4][5];
    int     ltp_scale_q14;
} frame_ctrl_t;

typedef struct {
    int fs_khz;
    int nb_subfr;
    int subfr_len;
    int frame_len;
    int order;
    int frames_per_packet;
    int frames_decoded;
    int vad[3];
    int lbrr_flag;
    int lbrr[3];
    bool first_frame_after_reset;
    int last_gain_index;
    int lag_prev;
    int prev_signal_type;
    int ec_prev_signal_type;
    int ec_prev_lag_index;
    int16_t prev_nlsf[MAX_ORDER];
    indices_t ind;
    int16_t out[LTP_MEM + 2 * MAX_SUBFR];
    int32_t lpc_q14[MAX_ORDER];
    int32_t prev_gain_q16;
    frame_ctrl_t last;
    int32_t  last_exc_q14;
    int      loss_count;
    uint32_t plc_seed;
    resampler_t rs;
} channel_t;

struct silk_dec {
    int channels;
    int channels_internal;
    int prev_decode_only_middle;
    int32_t pred_prev_q13[2];
    int16_t s_mid[2];
    int16_t s_side[2];
    channel_t ch[2];
    int16_t buf[2][MAX_FRAME + 2];
    int16_t resampled[MAX_FRAME * 6];
    int16_t upsampled[2 * 160 + 8];
    int32_t res[MAX_SUBFR];
    int pulses[MAX_FRAME + 16];
    int32_t exc_q14[MAX_FRAME];
    int16_t ltp[LTP_MEM];
    int32_t ltp_q15[LTP_MEM + MAX_FRAME];
    int32_t lpc_q14[MAX_ORDER + MAX_SUBFR];
};

static void resampler_init(resampler_t *r, int fs_in_khz) {
    memset(r, 0, sizeof(*r));
    r->fs_in_khz = fs_in_khz;
    r->input_delay = fs_in_khz == 8 ? 0 : fs_in_khz == 12 ? 4 : 7;
    const int32_t fs_in = fs_in_khz * 1000;
    int32_t inv = ((fs_in << 15) / 48000) << 2;
    while ((int32_t)(((int64_t)inv * 48000) >> 16) < fs_in << 1) inv++;
    r->inv_ratio_q16 = inv;
}

static inline int32_t mulwb(int32_t a, int16_t b) {
    return (int32_t)(((int64_t)a * b) >> 16);
}

static inline int16_t sat16(int32_t v) {
    return (int16_t)(v > 32767 ? 32767 : v < -32768 ? -32768 : v);
}

static void up2(int32_t *s, int16_t *out, const int16_t *in, int len) {
    for (int k = 0; k < len; k++) {
        const int32_t x = (int32_t)in[k] << 10;
        for (int branch = 0; branch < 2; branch++) {
            const int16_t *c = silk_up2_allpass[branch];
            int32_t *st = s + 3 * branch;
            int32_t y = x - st[0];
            int32_t t = mulwb(y, c[0]);
            const int32_t o1 = st[0] + t;
            st[0] = x + t;
            y = o1 - st[1];
            t = mulwb(y, c[1]);
            const int32_t o2 = st[1] + t;
            st[1] = o1 + t;
            y = o2 - st[2];
            t = y + mulwb(y, c[2]);
            const int32_t o3 = st[2] + t;
            st[2] = o2 + t;
            out[2 * k + branch] = sat16((o3 + 512) >> 10);
        }
    }
}

static int16_t *interpolate(int16_t *out, const int16_t *buf, int32_t max_index, int32_t step) {
    for (int32_t index = 0; index < max_index; index += step) {
        const int phase = (int)(((index & 0xFFFF) * 12) >> 16);
        const int16_t *p = buf + (index >> 16);
        const int16_t *lo = silk_frac_fir[phase];
        const int16_t *hi = silk_frac_fir[11 - phase];
        const int32_t v = p[0] * lo[0] + p[1] * lo[1] + p[2] * lo[2] + p[3] * lo[3] +
                          p[4] * hi[3] + p[5] * hi[2] + p[6] * hi[1] + p[7] * hi[0];
        *out++ = sat16((v + 16384) >> 15);
    }
    return out;
}

static int16_t *resample_block(resampler_t *r, int16_t *buf, int16_t *out, const int16_t *in, int len) {
    memcpy(buf, r->fir, sizeof(r->fir));
    const int batch = r->fs_in_khz * 10;
    for (;;) {
        const int n = len < batch ? len : batch;
        up2(r->iir, buf + 8, in, n);
        out = interpolate(out, buf, (int32_t)n << 17, r->inv_ratio_q16);
        in += n;
        len -= n;
        if (len <= 0) {
            memcpy(r->fir, buf + 2 * n, sizeof(r->fir));
            return out;
        }
        memmove(buf, buf + 2 * n, 8 * sizeof(int16_t));
    }
}

/* buf holds 2 * 160 + 8 samples. */
static void resample(resampler_t *r, int16_t *buf, int16_t *out, const int16_t *in, int len) {
    const int ms = r->fs_in_khz;
    const int fresh = ms - r->input_delay;
    memcpy(r->delay + r->input_delay, in, (size_t)fresh * sizeof(int16_t));
    out = resample_block(r, buf, out, r->delay, ms);
    resample_block(r, buf, out, in + fresh, len - ms);
    memcpy(r->delay, in + len - r->input_delay, (size_t)r->input_delay * sizeof(int16_t));
}

static void channel_init(channel_t *c) {
    memset(c, 0, sizeof(*c));
    c->first_frame_after_reset = true;
    c->last_gain_index = 10;
    c->lag_prev = 100;
    c->prev_gain_q16 = 65536;
}

static void channel_set_fs(channel_t *c, int fs_khz) {
    c->subfr_len = 5 * fs_khz;
    const int frame_len = c->nb_subfr * c->subfr_len;
    if (c->fs_khz != fs_khz) resampler_init(&c->rs, fs_khz);
    if (c->fs_khz != fs_khz || frame_len != c->frame_len) {
        if (c->fs_khz != fs_khz) {
            c->order = fs_khz == 16 ? 16 : 10;
            c->first_frame_after_reset = true;
            c->lag_prev = 100;
            c->last_gain_index = 10;
            c->prev_signal_type = TYPE_INACTIVE;
            memset(c->out, 0, sizeof(c->out));
            memset(c->lpc_q14, 0, sizeof(c->lpc_q14));
        }
        c->fs_khz = fs_khz;
        c->frame_len = frame_len;
    }
}

static void decode_indices(channel_t *c, opus_rc_t *rc, int frame, bool lbrr, int cond) {
    indices_t *ind = &c->ind;
    const bool wb = c->fs_khz == 16;
    const int type = lbrr || c->vad[frame] ? opus_rc_icdf(rc, silk_type_active_icdf, 8) + 2
                                            : opus_rc_icdf(rc, silk_type_inactive_icdf, 8);
    ind->signal_type = type >> 1;
    ind->offset_type = type & 1;

    if (cond == CODE_CONDITIONALLY) {
        ind->gains[0] = opus_rc_icdf(rc, silk_delta_gain_icdf, 8);
    } else {
        ind->gains[0] = opus_rc_icdf(rc, silk_gain_msb_icdf[ind->signal_type], 8) << 3;
        ind->gains[0] += opus_rc_icdf(rc, silk_uniform8_icdf, 8);
    }
    for (int i = 1; i < c->nb_subfr; i++) ind->gains[i] = opus_rc_icdf(rc, silk_delta_gain_icdf, 8);

    const int i1 = opus_rc_icdf(rc, silk_lsf_s1_icdf[(ind->signal_type >> 1) + 2 * wb], 8);
    ind->nlsf[0] = i1;
    for (int k = 0; k < c->order; k++) {
        const int book = wb ? 8 + silk_lsf_sel_wb[i1][k] : silk_lsf_sel_nbmb[i1][k];
        int ix = opus_rc_icdf(rc, silk_lsf_s2_icdf[book], 8);
        if (ix == 0) {
            ix -= opus_rc_icdf(rc, silk_lsf_ext_icdf, 8);
        } else if (ix == 8) {
            ix += opus_rc_icdf(rc, silk_lsf_ext_icdf, 8);
        }
        ind->nlsf[k + 1] = ix - 4;
    }
    ind->interp = c->nb_subfr == 4 ? opus_rc_icdf(rc, silk_lsf_interp_icdf, 8) : 4;

    if (ind->signal_type == TYPE_VOICED) {
        bool absolute = true;
        if (cond == CODE_CONDITIONALLY && c->ec_prev_signal_type == TYPE_VOICED) {
            const int delta = opus_rc_icdf(rc, silk_pitch_delta_icdf, 8);
            if (delta > 0) {
                ind->lag_index = c->ec_prev_lag_index + delta - 9;
                absolute = false;
            }
        }
        if (absolute) {
            const uint8_t *low = c->fs_khz == 8 ? silk_pitch_low_nb_icdf
                               : c->fs_khz == 12 ? silk_pitch_low_mb_icdf : silk_pitch_low_wb_icdf;
            ind->lag_index = opus_rc_icdf(rc, silk_pitch_high_icdf, 8) * (c->fs_khz >> 1);
            ind->lag_index += opus_rc_icdf(rc, low, 8);
        }
        c->ec_prev_lag_index = ind->lag_index;
        const uint8_t *contour = c->fs_khz == 8 ? (c->nb_subfr == 4 ? silk_contour_nb20_icdf : silk_contour_nb10_icdf)
                                                : (c->nb_subfr == 4 ? silk_contour_wb20_icdf : silk_contour_wb10_icdf);
        ind->contour = opus_rc_icdf(rc, contour, 8);
        ind->per_index = opus_rc_icdf(rc, silk_ltp_per_icdf, 8);
        static const uint8_t *const filters[3] = { silk_ltp_filter0_icdf, silk_ltp_filter1_icdf,
                                                   silk_ltp_filter2_icdf };
        for (int k = 0; k < c->nb_subfr; k++) ind->ltp_index[k] = opus_rc_icdf(rc, filters[ind->per_index], 8);
        ind->ltp_scale = cond == CODE_INDEPENDENTLY ? opus_rc_icdf(rc, silk_ltp_scale_icdf, 8) : 0;
    }
    c->ec_prev_signal_type = ind->signal_type;
    ind->seed = opus_rc_icdf(rc, silk_uniform4_icdf, 8);
}

static void shell_split(opus_rc_t *rc, int *a, int *b, int p, int table) {
    if (p > 0) {
        *a = opus_rc_icdf(rc, silk_shell_icdf[table][p - 1], 8);
        *b = p - *a;
    } else {
        *a = *b = 0;
    }
}

static void shell_decode(opus_rc_t *rc, int *out, int p16) {
    int p8[2], p4[4], p2[8];
    shell_split(rc, &p8[0], &p8[1], p16, 0);
    for (int i = 0; i < 2; i++) {
        shell_split(rc, &p4[2 * i], &p4[2 * i + 1], p8[i], 1);
        for (int j = 2 * i; j < 2 * i + 2; j++) {
            shell_split(rc, &p2[2 * j], &p2[2 * j + 1], p4[j], 2);
            for (int k = 2 * j; k < 2 * j + 2; k++) shell_split(rc, &out[2 * k], &out[2 * k + 1], p2[k], 3);
        }
    }
}

static void decode_pulses(opus_rc_t *rc, int *pulses, int signal_type, int offset_type, int frame_len) {
    int sums[20], lsbs[20];
    const int rate = opus_rc_icdf(rc, silk_rate_level_icdf[signal_type >> 1], 8);
    const int blocks = (frame_len + 15) >> 4;
    for (int i = 0; i < blocks; i++) {
        lsbs[i] = 0;
        sums[i] = opus_rc_icdf(rc, silk_pulse_count_icdf[rate], 8);
        while (sums[i] == 17) {
            lsbs[i]++;
            sums[i] = opus_rc_icdf(rc, silk_pulse_count_icdf[lsbs[i] == 10 ? 10 : 9], 8);
        }
    }
    for (int i = 0; i < blocks; i++) {
        if (sums[i] > 0) {
            shell_decode(rc, pulses + 16 * i, sums[i]);
        } else {
            memset(pulses + 16 * i, 0, 16 * sizeof(int));
        }
    }
    for (int i = 0; i < blocks; i++) {
        if (!lsbs[i]) continue;
        for (int k = 0; k < 16; k++) {
            int v = pulses[16 * i + k];
            for (int j = 0; j < lsbs[i]; j++) v = (v << 1) + opus_rc_icdf(rc, silk_lsb_icdf, 8);
            pulses[16 * i + k] = v;
        }
    }
    const uint8_t *sign = silk_sign_icdf + 7 * (2 * signal_type + offset_type);
    for (int i = 0; i < blocks; i++) {
        if (sums[i] == 0 && lsbs[i] == 0) continue;
        const uint8_t icdf[2] = { sign[sums[i] < 6 ? sums[i] : 6], 0 };
        for (int k = 0; k < 16; k++) {
            int *q = &pulses[16 * i + k];
            if (*q > 0 && opus_rc_icdf(rc, icdf, 8) == 0) *q = -*q;
        }
    }
}

static int32_t log2lin(int32_t in_log_q7) {
    if (in_log_q7 < 0) return 0;
    if (in_log_q7 >= 3967) return INT32_MAX;
    const int32_t i = in_log_q7 >> 7;
    const int32_t f = in_log_q7 & 127;
    const int32_t out = 1 << i;
    const int32_t frac = (-174 * f * (128 - f) >> 16) + f;
    return i < 16 ? out + ((out * frac) >> 7) : out + (out >> 7) * frac;
}

static void dequant_gains(channel_t *c, int32_t *gain_q16, bool conditional) {
    int prev = c->last_gain_index;
    for (int k = 0; k < c->nb_subfr; k++) {
        const int g = c->ind.gains[k];
        if (k == 0 && !conditional) {
            prev = g > prev - 16 ? g : prev - 16;
        } else {
            const int a = 2 * g - 16;
            const int b = prev + g - 4;
            prev = a > b ? a : b;
        }
        if (prev < 0) prev = 0;
        if (prev > 63) prev = 63;
        gain_q16[k] = log2lin(((0x1D1C71 * prev) >> 16) + 2090);
    }
    c->last_gain_index = prev;
}

static void decode_nlsf(const channel_t *c, int16_t *nlsf) {
    const bool wb = c->fs_khz == 16;
    const int d = c->order;
    const int i1 = c->ind.nlsf[0];
    const uint8_t *cb = wb ? silk_lsf_cb_wb[i1] : silk_lsf_cb_nbmb[i1];
    const int qstep = wb ? 9830 : 11796;
    int res_q10[MAX_ORDER];
    for (int k = d - 1; k >= 0; k--) {
        int pred = 0;
        if (k + 1 < d) {
            const int sel = wb ? silk_lsf_pred_sel_wb[i1][k] : silk_lsf_pred_sel_nbmb[i1][k];
            const uint8_t w = wb ? silk_lsf_pred_wb[sel][k] : silk_lsf_pred_nbmb[sel][k];
            pred = (res_q10[k + 1] * w) >> 8;
        }
        const int i2 = c->ind.nlsf[k + 1];
        const int v = (i2 << 10) - (i2 > 0 ? 102 : i2 < 0 ? -102 : 0);
        res_q10[k] = pred + ((v * qstep) >> 16);
    }
    for (int k = 0; k < d; k++) {
        const int prev = k > 0 ? cb[k - 1] : 0;
        const int next = k + 1 < d ? cb[k + 1] : 256;
        const int32_t w2 = (1024 / (cb[k] - prev) + 1024 / (next - cb[k])) << 16;
        const int i = opus_ilog((uint32_t)w2);
        const int f = (w2 >> (i - 8)) & 127;
        const int y = ((i & 1) ? 32768 : 46214) >> ((32 - i) >> 1);
        const int w_q9 = y + ((213 * f * y) >> 16);
        int v = (cb[k] << 7) + (res_q10[k] << 14) / w_q9;
        nlsf[k] = (int16_t)(v < 0 ? 0 : v > 32767 ? 32767 : v);
    }
}

static void stabilize_nlsf(int16_t *nlsf, const int16_t *min_delta, int d) {
    for (int round = 0; round < 20; round++) {
        int lowest = INT32_MAX;
        int at = 0;
        for (int i = 0; i <= d; i++) {
            const int prev = i > 0 ? nlsf[i - 1] : 0;
            const int cur = i < d ? nlsf[i] : 32768;
            const int diff = cur - prev - min_delta[i];
            if (diff < lowest) {
                lowest = diff;
                at = i;
            }
        }
        if (lowest >= 0) return;
        if (at == 0) {
            nlsf[0] = min_delta[0];
        } else if (at == d) {
            nlsf[d - 1] = (int16_t)(32768 - min_delta[d]);
        } else {
            int min_center = min_delta[at] >> 1;
            for (int k = 0; k < at; k++) min_center += min_delta[k];
            int max_center = 32768 - (min_delta[at] >> 1);
            for (int k = d; k > at; k--) max_center -= min_delta[k];
            int center = (nlsf[at - 1] + nlsf[at] + 1) >> 1;
            if (center < min_center) center = min_center;
            if (center > max_center) center = max_center;
            nlsf[at - 1] = (int16_t)(center - (min_delta[at] >> 1));
            nlsf[at] = (int16_t)(nlsf[at - 1] + min_delta[at]);
        }
    }
    for (int i = 1; i < d; i++) {
        const int16_t v = nlsf[i];
        int j = i - 1;
        while (j >= 0 && nlsf[j] > v) {
            nlsf[j + 1] = nlsf[j];
            j--;
        }
        nlsf[j + 1] = v;
    }
    for (int k = 0; k < d; k++) {
        int floor_v = (k > 0 ? nlsf[k - 1] : 0) + min_delta[k];
        if (floor_v > 32767) floor_v = 32767;
        if (nlsf[k] < floor_v) nlsf[k] = (int16_t)floor_v;
    }
    for (int k = d - 1; k >= 0; k--) {
        const int ceil_v = (k + 1 < d ? nlsf[k + 1] : 32768) - min_delta[k + 1];
        if (nlsf[k] > ceil_v) nlsf[k] = (int16_t)ceil_v;
    }
}

static void bwexpand32(int32_t *a, int d, int32_t chirp_q16) {
    const int32_t c0 = chirp_q16;
    for (int i = 0; i < d - 1; i++) {
        a[i] = (int32_t)(((int64_t)chirp_q16 * a[i]) >> 16);
        chirp_q16 = (int32_t)(((int64_t)c0 * chirp_q16 + 32768) >> 16);
    }
    a[d - 1] = (int32_t)(((int64_t)chirp_q16 * a[d - 1]) >> 16);
}

/* RFC 6716 4.2.7.5.8, with the RFC 8251 overflow check: false if unstable. */
static bool lpc_stable(const int16_t *a_q12, int d) {
    int32_t a[2][MAX_ORDER];
    int32_t dc = 0;
    int32_t *cur = a[d & 1];
    for (int k = 0; k < d; k++) {
        dc += a_q12[k];
        cur[k] = (int32_t)a_q12[k] << 12;
    }
    if (dc >= 4096) return false;
    int64_t inv_gain = 1 << 30;
    for (int k = d - 1; k >= 0; k--) {
        if (cur[k] > 16773022 || cur[k] < -16773022) return false;
        const int32_t rc = -(cur[k] * 128);
        const int32_t div = (int32_t)((1 << 30) - (((int64_t)rc * rc) >> 32));
        inv_gain = (((inv_gain * div) >> 32) << 2);
        if (inv_gain < 107374) return false;
        if (k == 0) break;
        const int b1 = opus_ilog((uint32_t)div);
        const int b2 = b1 - 16;
        const int32_t inv = ((1 << 29) - 1) / (div >> (b2 + 1));
        const int32_t err = (1 << 29) - (int32_t)(((int64_t)(div << (15 - b2)) * inv) >> 16);
        const int32_t gain = (inv << 16) + (int32_t)(((int64_t)err * inv) >> 13);
        int32_t *next = a[k & 1];
        for (int n = 0; n < k; n++) {
            const int64_t t = (int64_t)cur[n] - ((((int64_t)cur[k - n - 1] * rc) + (1 << 30)) >> 31);
            const int32_t num = (int32_t)(t > INT32_MAX ? INT32_MAX : t < INT32_MIN ? INT32_MIN : t);
            const int64_t v = (((int64_t)num * gain) + ((int64_t)1 << (b1 - 1))) >> b1;
            if (v > INT32_MAX || v < INT32_MIN) return false;
            next[n] = (int32_t)v;
        }
        cur = next;
    }
    return true;
}

static void nlsf_to_lpc(const int16_t *nlsf, int d, int16_t *a_q12) {
    const uint8_t *order = d == 16 ? silk_lsf_order_wb : silk_lsf_order_nbmb;
    int32_t c[MAX_ORDER];
    for (int k = 0; k < d; k++) {
        const int i = nlsf[k] >> 8;
        const int f = nlsf[k] & 255;
        c[order[k]] = (silk_lsf_cos_q12[i] * 256 + (silk_lsf_cos_q12[i + 1] - silk_lsf_cos_q12[i]) * f + 4) >> 3;
    }
    const int d2 = d / 2;
    int32_t p[MAX_ORDER / 2 + 2], q[MAX_ORDER / 2 + 2];
    for (int pass = 0; pass < 2; pass++) {
        int32_t *poly = pass ? q : p;
        const int32_t *cs = c + pass;
        poly[0] = 1 << 16;
        poly[1] = -cs[0];
        for (int k = 1; k < d2; k++) {
            const int32_t ck = cs[2 * k];
            poly[k + 1] = 2 * poly[k - 1] - (int32_t)(((int64_t)ck * poly[k] + 32768) >> 16);
            for (int j = k; j > 1; j--) {
                poly[j] += poly[j - 2] - (int32_t)(((int64_t)ck * poly[j - 1] + 32768) >> 16);
            }
            poly[1] -= ck;
        }
    }
    int32_t a32[MAX_ORDER];
    for (int k = 0; k < d2; k++) {
        const int32_t pt = p[k + 1] + p[k];
        const int32_t qt = q[k + 1] - q[k];
        a32[k] = -qt - pt;
        a32[d - k - 1] = qt - pt;
    }
    int round;
    for (round = 0; round < 10; round++) {
        int32_t maxabs = 0;
        int at = 0;
        for (int k = 0; k < d; k++) {
            const int32_t v = a32[k] < 0 ? -a32[k] : a32[k];
            if (v > maxabs) {
                maxabs = v;
                at = k;
            }
        }
        maxabs = (maxabs + 16) >> 5;
        if (maxabs <= 32767) break;
        if (maxabs > 163838) maxabs = 163838;
        const int32_t sc = 65470 - ((maxabs - 32767) << 14) / ((maxabs * (at + 1)) >> 2);
        bwexpand32(a32, d, sc);
    }
    if (round == 10) {
        for (int k = 0; k < d; k++) {
            a_q12[k] = sat16((a32[k] + 16) >> 5);
            a32[k] = (int32_t)a_q12[k] << 5;
        }
    } else {
        for (int k = 0; k < d; k++) a_q12[k] = (int16_t)((a32[k] + 16) >> 5);
    }
    for (int i = 0; i < 16; i++) {
        if (lpc_stable(a_q12, d)) break;
        bwexpand32(a32, d, 65536 - (2 << i));
        for (int k = 0; k < d; k++) a_q12[k] = (int16_t)((a32[k] + 16) >> 5);
    }
}

static void decode_parameters(channel_t *c, frame_ctrl_t *ctl, int cond) {
    dequant_gains(c, ctl->gain_q16, cond == CODE_CONDITIONALLY);
    int16_t nlsf[MAX_ORDER];
    decode_nlsf(c, nlsf);
    stabilize_nlsf(nlsf, c->order == 16 ? silk_lsf_min_spacing_wb : silk_lsf_min_spacing_nbmb, c->order);
    nlsf_to_lpc(nlsf, c->order, ctl->a_q12[1]);
    if (c->first_frame_after_reset) c->ind.interp = 4;
    if (c->ind.interp < 4) {
        int16_t mid[MAX_ORDER];
        for (int i = 0; i < c->order; i++) {
            mid[i] = (int16_t)(c->prev_nlsf[i] + ((c->ind.interp * (nlsf[i] - c->prev_nlsf[i])) >> 2));
        }
        nlsf_to_lpc(mid, c->order, ctl->a_q12[0]);
    } else {
        memcpy(ctl->a_q12[0], ctl->a_q12[1], sizeof(ctl->a_q12[1]));
    }
    memcpy(c->prev_nlsf, nlsf, (size_t)c->order * sizeof(int16_t));

    if (c->ind.signal_type == TYPE_VOICED) {
        const int min_lag = 2 * c->fs_khz;
        const int max_lag = 18 * c->fs_khz;
        const int lag = min_lag + c->ind.lag_index;
        for (int k = 0; k < c->nb_subfr; k++) {
            int off;
            if (c->fs_khz == 8) {
                off = c->nb_subfr == 4 ? silk_contour_nb20[c->ind.contour][k] : silk_contour_nb10[c->ind.contour][k];
            } else {
                off = c->nb_subfr == 4 ? silk_contour_wb20[c->ind.contour][k] : silk_contour_wb10[c->ind.contour][k];
            }
            int v = lag + off;
            ctl->pitch[k] = v < min_lag ? min_lag : v > max_lag ? max_lag : v;
        }
        static const int8_t (*const taps[3])[5] = { silk_ltp_taps0, silk_ltp_taps1, silk_ltp_taps2 };
        for (int k = 0; k < c->nb_subfr; k++) memcpy(ctl->ltp_q7[k], taps[c->ind.per_index][c->ind.ltp_index[k]], 5);
        static const int16_t scales[3] = { 15565, 12288, 8192 };
        ctl->ltp_scale_q14 = scales[c->ind.ltp_scale];
    } else {
        memset(ctl->pitch, 0, sizeof(ctl->pitch));
        memset(ctl->ltp_q7, 0, sizeof(ctl->ltp_q7));
        ctl->ltp_scale_q14 = 0;
    }
}

/* Reconstruction uses the fixed-point formats of the RFC's reference decoder:
 * the encoder quantizes against that exact filter state, so a decoder that
 * rounds differently drifts audibly over long voiced stretches. */

static inline int32_t smulwb(int32_t a, int32_t b) {
    return (int32_t)(((int64_t)a * (int16_t)b) >> 16);
}

static inline int32_t smulww(int32_t a, int32_t b) {
    return (int32_t)(((int64_t)a * b) >> 16);
}

static inline int32_t rshift_round(int64_t a, int shift) {
    return (int32_t)((a + ((int64_t)1 << (shift - 1))) >> shift);
}

static inline int32_t sat32(int64_t v) {
    return v > INT32_MAX ? INT32_MAX : v < INT32_MIN ? INT32_MIN : (int32_t)v;
}

static int32_t lshift_sat(int32_t a, int shift) {
    const int64_t v = (int64_t)a << shift;
    return v > INT32_MAX ? INT32_MAX : v < INT32_MIN ? INT32_MIN : (int32_t)v;
}

static int32_t inverse_varq(int32_t b, int qres) {
    const int headroom = 32 - opus_ilog((uint32_t)(b < 0 ? -b : b)) - 1;
    const int32_t nrm = b << headroom;
    const int32_t inv = (INT32_MAX >> 2) / (nrm >> 16);
    int32_t result = inv << 16;
    const int32_t err_q32 = ((1 << 29) - smulwb(nrm, inv)) << 3;
    result += (int32_t)(((int64_t)err_q32 * inv) >> 16);
    const int shift = 61 - headroom - qres;
    if (shift <= 0) return lshift_sat(result, -shift);
    return shift < 32 ? result >> shift : 0;
}

static int32_t div_varq(int32_t a, int32_t b, int qres) {
    const int a_head = 32 - opus_ilog((uint32_t)(a < 0 ? -a : a)) - 1;
    int32_t a_nrm = a << a_head;
    const int b_head = 32 - opus_ilog((uint32_t)(b < 0 ? -b : b)) - 1;
    const int32_t b_nrm = b << b_head;
    const int32_t inv = (INT32_MAX >> 2) / (b_nrm >> 16);
    int32_t result = smulwb(a_nrm, inv);
    a_nrm = (int32_t)((uint32_t)a_nrm - ((uint32_t)(int32_t)(((int64_t)b_nrm * result) >> 32) << 3));
    result += smulwb(a_nrm, inv);
    const int shift = 29 + a_head - b_head - qres;
    if (shift < 0) return lshift_sat(result, -shift);
    return shift < 32 ? result >> shift : 0;
}

/* out[i] for d <= i < len is in[i] minus its prediction from in[i - d .. i - 1]. */
static void analysis_filter(int16_t *out, const int16_t *in, const int16_t *a_q12, int len, int d) {
    for (int i = d; i < len; i++) {
        uint32_t acc = 0;
        for (int k = 0; k < d; k++) acc += (uint32_t)((int32_t)in[i - k - 1] * a_q12[k]);
        const int32_t v = (int32_t)(((uint32_t)in[i] << 12) - acc);
        out[i] = sat16(rshift_round(v, 12));
    }
    memset(out, 0, (size_t)d * sizeof(*out));
}

static void excitation(silk_dec_t *s, const channel_t *c) {
    static const int offsets_q10[3][2] = { { 100, 240 }, { 100, 240 }, { 32, 100 } };
    const int offset = offsets_q10[c->ind.signal_type][c->ind.offset_type] << 4;
    uint32_t seed = (uint32_t)c->ind.seed;
    for (int i = 0; i < c->frame_len; i++) {
        const int raw = s->pulses[i];
        int32_t v = raw * (1 << 14);
        if (v > 0) v -= 80 << 4;
        if (v < 0) v += 80 << 4;
        v += offset;
        seed = 907633515u + seed * 196314165u;
        if (seed & 0x80000000u) v = -v;
        s->exc_q14[i] = v;
        seed += (uint32_t)raw;
    }
}

/* LTP and LPC synthesis of one frame from s->exc_q14 into pcm. */
static void synthesize(silk_dec_t *s, channel_t *c, frame_ctrl_t *ctl, int signal_type, bool interp,
                       int16_t *pcm) {
    const int n = c->subfr_len;
    const int d = c->order;
    const int ltp_mem = 20 * c->fs_khz;
    int32_t *lpc = s->lpc_q14;
    int ltp_index = ltp_mem;
    memcpy(lpc, c->lpc_q14, sizeof(c->lpc_q14));

    for (int k = 0; k < c->nb_subfr; k++) {
        const int16_t *a = ctl->a_q12[k >> 1];
        int8_t *b = ctl->ltp_q7[k];
        int type = signal_type;
        const int32_t gain_q16 = ctl->gain_q16[k];
        const int32_t gain_q10 = gain_q16 >> 6;
        int32_t inv_gain_q31 = inverse_varq(gain_q16, 47);
        int32_t gain_adj_q16 = 1 << 16;
        if (gain_q16 != c->prev_gain_q16) {
            gain_adj_q16 = div_varq(c->prev_gain_q16, gain_q16, 16);
            for (int i = 0; i < MAX_ORDER; i++) lpc[i] = smulww(gain_adj_q16, lpc[i]);
        }
        c->prev_gain_q16 = gain_q16;

        if (c->loss_count && c->prev_signal_type == TYPE_VOICED && type != TYPE_VOICED && k < 2) {
            memset(b, 0, 5);
            b[2] = 32;
            type = TYPE_VOICED;
            ctl->pitch[k] = c->lag_prev;
        }

        const int32_t *res = s->exc_q14 + k * n;
        int32_t *res_buf = s->res;
        if (type == TYPE_VOICED) {
            const int lag = ctl->pitch[k];
            if (k == 0 || (k == 2 && interp)) {
                const int start = ltp_mem - lag - d - 2;
                if (k == 2) memcpy(c->out + ltp_mem, pcm, (size_t)2 * n * sizeof(int16_t));
                analysis_filter(s->ltp + start, c->out + start + k * n, a, ltp_mem - start, d);
                if (k == 0) inv_gain_q31 = smulwb(inv_gain_q31, ctl->ltp_scale_q14) << 2;
                for (int i = 0; i < lag + 2; i++) {
                    s->ltp_q15[ltp_index - i - 1] = smulwb(inv_gain_q31, s->ltp[ltp_mem - i - 1]);
                }
            } else if (gain_adj_q16 != 1 << 16) {
                for (int i = 0; i < lag + 2; i++) {
                    s->ltp_q15[ltp_index - i - 1] = smulww(gain_adj_q16, s->ltp_q15[ltp_index - i - 1]);
                }
            }
            const int32_t *pred = s->ltp_q15 + ltp_index - lag + 2;
            for (int i = 0; i < n; i++) {
                int32_t p = 2;
                for (int j = 0; j < 5; j++) p += smulwb(pred[i - j], (int32_t)b[j] << 7);
                res_buf[i] = s->exc_q14[k * n + i] + p * 2;
                s->ltp_q15[ltp_index++] = res_buf[i] * 2;
            }
            res = res_buf;
        }

        for (int i = 0; i < n; i++) {
            int32_t p = d >> 1;
            for (int j = 0; j < d; j++) p += smulwb(lpc[MAX_ORDER + i - j - 1], a[j]);
            lpc[MAX_ORDER + i] = sat32((int64_t)res[i] + lshift_sat(p, 4));
            pcm[k * n + i] = sat16(rshift_round(smulww(lpc[MAX_ORDER + i], gain_q10), 8));
        }
        memmove(lpc, lpc + n, MAX_ORDER * sizeof(int32_t));
    }
    memcpy(c->lpc_q14, lpc, sizeof(c->lpc_q14));

    const int keep = ltp_mem - c->frame_len;
    memmove(c->out, c->out + c->frame_len, (size_t)keep * sizeof(int16_t));
    memcpy(c->out + keep, pcm, (size_t)c->frame_len * sizeof(int16_t));
}

/* Not specified by RFC 6716: the last frame's filters run on again, voiced
 * with its pitch predictor and no new excitation, otherwise on noise at the
 * last excitation level, fading out frame by frame. */
static void conceal(silk_dec_t *s, channel_t *c, int16_t *pcm) {
    frame_ctrl_t ctl = c->last;
    const int fade = c->loss_count == 0 ? 58982 : 45875;
    for (int k = 0; k < 4; k++) {
        ctl.gain_q16[k] = c->last.gain_q16[c->nb_subfr == 4 ? 3 : 1];
        ctl.pitch[k] = c->lag_prev;
        memcpy(ctl.ltp_q7[k], c->last.ltp_q7[c->nb_subfr == 4 ? 3 : 1], 5);
    }
    memcpy(ctl.a_q12[0], c->last.a_q12[1], sizeof(ctl.a_q12[1]));
    ctl.ltp_scale_q14 = 16384;
    const int32_t level = c->last_exc_q14;
    for (int i = 0; i < c->frame_len; i++) {
        c->plc_seed = 907633515u + c->plc_seed * 196314165u;
        s->exc_q14[i] = c->prev_signal_type == TYPE_VOICED ? 0 : (int32_t)(((int64_t)(int32_t)c->plc_seed * level) >> 31);
    }
    for (int k = 0; k < 4; k++) {
        if (c->prev_signal_type == TYPE_VOICED) {
            for (int j = 0; j < 5; j++) ctl.ltp_q7[k][j] = (int8_t)(ctl.ltp_q7[k][j] * fade >> 16);
        }
    }
    int32_t chirp = 64880;
    for (int i = 0; i < c->order; i++) {
        ctl.a_q12[0][i] = ctl.a_q12[1][i] = (int16_t)rshift_round((int64_t)chirp * ctl.a_q12[1][i], 16);
        chirp += rshift_round((int64_t)chirp * (chirp - 65536), 16);
    }
    c->last = ctl;
    for (int k = 0; k < 4; k++) c->last.gain_q16[k] = (int32_t)(((int64_t)ctl.gain_q16[k] * fade) >> 16);
    c->last_exc_q14 = (int32_t)(((int64_t)level * fade) >> 16);
    synthesize(s, c, &ctl, c->prev_signal_type == TYPE_VOICED ? TYPE_VOICED : TYPE_UNVOICED, false, pcm);
    c->loss_count++;
}

static int decode_frame(silk_dec_t *s, channel_t *c, opus_rc_t *rc, bool lost, int cond, int16_t *pcm) {
    if (!lost) {
        frame_ctrl_t ctl;
        decode_indices(c, rc, c->frames_decoded, false, cond);
        decode_pulses(rc, s->pulses, c->ind.signal_type, c->ind.offset_type, c->frame_len);
        decode_parameters(c, &ctl, cond);
        excitation(s, c);
        synthesize(s, c, &ctl, c->ind.signal_type, c->nb_subfr == 4 && c->ind.interp < 4, pcm);
        int64_t energy = 0;
        for (int i = 0; i < c->frame_len; i++) energy += (int64_t)(s->exc_q14[i] >> 4) * (s->exc_q14[i] >> 4);
        c->last_exc_q14 = (int32_t)sqrtf((float)energy / (float)c->frame_len) << 4;
        c->last = ctl;
        c->loss_count = 0;
        c->prev_signal_type = c->ind.signal_type;
        c->first_frame_after_reset = false;
        c->lag_prev = ctl.pitch[c->nb_subfr - 1];
    } else {
        conceal(s, c, pcm);
    }
    return c->frame_len;
}

static void decode_stereo_pred(opus_rc_t *rc, int32_t *pred_q13) {
    const int n = opus_rc_icdf(rc, silk_stereo_s1_icdf, 8);
    int ix[2][3];
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for (int i = 0; i < 2; i++) {
        ix[i][0] = opus_rc_icdf(rc, silk_stereo_s2_icdf, 8);
        ix[i][1] = opus_rc_icdf(rc, silk_stereo_s3_icdf, 8);
    }
    for (int i = 0; i < 2; i++) {
        const int wi = ix[i][0] + 3 * ix[i][2];
        const int32_t low = silk_stereo_weights[wi];
        const int32_t step = ((silk_stereo_weights[wi + 1] - low) * 6554) >> 16;
        pred_q13[i] = low + step * (2 * ix[i][1] + 1);
    }
    pred_q13[0] -= pred_q13[1];
}

static void ms_to_lr(silk_dec_t *s, int16_t *mid, int16_t *side, const int32_t *pred_q13, int fs_khz, int len) {
    memcpy(mid, s->s_mid, sizeof(s->s_mid));
    memcpy(side, s->s_side, sizeof(s->s_side));
    memcpy(s->s_mid, mid + len, sizeof(s->s_mid));
    memcpy(s->s_side, side + len, sizeof(s->s_side));
    const int n1 = 8 * fs_khz;
    const int32_t denom_q16 = (1 << 16) / n1;
    const int32_t delta0 = rshift_round((int64_t)(int16_t)(pred_q13[0] - s->pred_prev_q13[0]) * (int16_t)denom_q16, 16);
    const int32_t delta1 = rshift_round((int64_t)(int16_t)(pred_q13[1] - s->pred_prev_q13[1]) * (int16_t)denom_q16, 16);
    int32_t w0 = s->pred_prev_q13[0];
    int32_t w1 = s->pred_prev_q13[1];
    for (int i = 0; i < len; i++) {
        if (i < n1) {
            w0 += delta0;
            w1 += delta1;
        } else {
            w0 = pred_q13[0];
            w1 = pred_q13[1];
        }
        const int32_t p0 = ((int32_t)mid[i] + mid[i + 2] + 2 * (int32_t)mid[i + 1]) << 9;
        int32_t v = ((int32_t)side[i + 1] << 8) + smulwb(p0, w0);
        v += smulwb((int32_t)mid[i + 1] << 11, w1);
        side[i + 1] = sat16(rshift_round(v, 8));
    }
    s->pred_prev_q13[0] = pred_q13[0];
    s->pred_prev_q13[1] = pred_q13[1];
    for (int i = 1; i <= len; i++) {
        const int32_t m = mid[i];
        const int32_t sd = side[i];
        mid[i] = sat16(m + sd);
        side[i] = sat16(m - sd);
    }
}

int silk_decode(silk_dec_t *s, opus_rc_t *rc, bool lost, bool first, int internal_khz,
                int stream_channels, int payload_ms, int16_t *out) {
    channel_t *ch = s->ch;
    int decode_only_middle = 0;
    int32_t pred_q13[2] = { 0, 0 };

    if (lost && ch[0].fs_khz) internal_khz = ch[0].fs_khz;
    if (first) {
        for (int n = 0; n < stream_channels; n++) ch[n].frames_decoded = 0;
    }
    if (stream_channels > s->channels_internal) channel_init(&ch[1]);
    const bool stereo_to_mono = stream_channels == 1 && s->channels_internal == 2 && internal_khz == ch[0].fs_khz;
    if (ch[0].frames_decoded == 0) {
        for (int n = 0; n < stream_channels; n++) {
            ch[n].frames_per_packet = payload_ms <= 20 ? 1 : payload_ms / 20;
            ch[n].nb_subfr = payload_ms == 10 ? 2 : 4;
            channel_set_fs(&ch[n], internal_khz);
        }
    }
    if (s->channels == 2 && stream_channels == 2 && s->channels_internal == 1) {
        s->pred_prev_q13[0] = s->pred_prev_q13[1] = 0;
        s->s_side[0] = s->s_side[1] = 0;
        ch[1].rs = ch[0].rs;
    }
    s->channels_internal = stream_channels;

    if (!lost && ch[0].frames_decoded == 0) {
        for (int n = 0; n < stream_channels; n++) {
            for (int i = 0; i < ch[n].frames_per_packet; i++) ch[n].vad[i] = opus_rc_bit_logp(rc, 1);
            ch[n].lbrr_flag = opus_rc_bit_logp(rc, 1);
        }
        for (int n = 0; n < stream_channels; n++) {
            memset(ch[n].lbrr, 0, sizeof(ch[n].lbrr));
            if (!ch[n].lbrr_flag) continue;
            if (ch[n].frames_per_packet == 1) {
                ch[n].lbrr[0] = 1;
            } else {
                const int sym = opus_rc_icdf(rc, silk_lbrr_icdf[ch[n].frames_per_packet - 2], 8) + 1;
                for (int i = 0; i < ch[n].frames_per_packet; i++) ch[n].lbrr[i] = (sym >> i) & 1;
            }
        }
        for (int i = 0; i < ch[0].frames_per_packet; i++) {
            for (int n = 0; n < stream_channels; n++) {
                if (!ch[n].lbrr[i]) continue;
                if (stream_channels == 2 && n == 0) {
                    decode_stereo_pred(rc, pred_q13);
                    if (!ch[1].lbrr[i]) decode_only_middle = opus_rc_icdf(rc, silk_mid_only_icdf, 8);
                }
                const int cond = i > 0 && ch[n].lbrr[i - 1] ? CODE_CONDITIONALLY : CODE_INDEPENDENTLY;
                decode_indices(&ch[n], rc, i, true, cond);
                decode_pulses(rc, s->pulses, ch[n].ind.signal_type, ch[n].ind.offset_type, ch[n].frame_len);
            }
        }
    }

    if (stream_channels == 2) {
        if (!lost) {
            decode_stereo_pred(rc, pred_q13);
            decode_only_middle = ch[1].vad[ch[0].frames_decoded] == 0 ? opus_rc_icdf(rc, silk_mid_only_icdf, 8) : 0;
        } else {
            pred_q13[0] = s->pred_prev_q13[0];
            pred_q13[1] = s->pred_prev_q13[1];
        }
    }
    if (stream_channels == 2 && decode_only_middle == 0 && s->prev_decode_only_middle == 1) {
        channel_t *side = &ch[1];
        memset(side->out, 0, sizeof(side->out));
        memset(side->lpc_q14, 0, sizeof(side->lpc_q14));
        side->lag_prev = 100;
        side->last_gain_index = 10;
        side->prev_signal_type = TYPE_INACTIVE;
        side->first_frame_after_reset = true;
    }
    const bool has_side = lost ? !s->prev_decode_only_middle : !decode_only_middle;

    int frame_len = ch[0].frame_len;
    for (int n = 0; n < stream_channels; n++) {
        if (n == 0 || has_side) {
            const int index = ch[0].frames_decoded - n;
            int cond;
            if (index <= 0) {
                cond = CODE_INDEPENDENTLY;
            } else if (n > 0 && s->prev_decode_only_middle) {
                cond = CODE_INDEPENDENTLY_NO_LTP_SCALING;
            } else {
                cond = CODE_CONDITIONALLY;
            }
            frame_len = decode_frame(s, &ch[n], rc, lost, cond, s->buf[n] + 2);
        } else {
            memset(s->buf[n] + 2, 0, (size_t)frame_len * sizeof(int16_t));
        }
        ch[n].frames_decoded++;
    }

    if (s->channels == 2 && stream_channels == 2) {
        ms_to_lr(s, s->buf[0], s->buf[1], pred_q13, ch[0].fs_khz, frame_len);
    } else {
        memcpy(s->buf[0], s->s_mid, sizeof(s->s_mid));
        memcpy(s->s_mid, s->buf[0] + frame_len, sizeof(s->s_mid));
    }

    const int out_len = frame_len * 48 / ch[0].fs_khz;
    const int ch_out = s->channels;
    for (int n = 0; n < (ch_out < stream_channels ? ch_out : stream_channels); n++) {
        resample(&ch[n].rs, s->upsampled, s->resampled, s->buf[n] + 1, frame_len);
        for (int i = 0; i < out_len; i++) out[i * ch_out + n] = s->resampled[i];
    }
    if (ch_out == 2 && stream_channels == 1) {
        if (stereo_to_mono) {
            resample(&ch[1].rs, s->upsampled, s->resampled, s->buf[0] + 1, frame_len);
            for (int i = 0; i < out_len; i++) out[i * 2 + 1] = s->resampled[i];
        } else {
            for (int i = 0; i < out_len; i++) out[i * 2 + 1] = out[i * 2];
        }
    }

    if (lost) {
        for (int n = 0; n < s->channels_internal; n++) ch[n].last_gain_index = 10;
    } else {
        s->prev_decode_only_middle = decode_only_middle;
    }
    return out_len;
}

void silk_reset(silk_dec_t *s) {
    const int channels = s->channels;
    memset(s, 0, sizeof(*s));
    s->channels = channels;
    channel_init(&s->ch[0]);
    channel_init(&s->ch[1]);
}

esp_err_t silk_create(silk_dec_t **out, int channels, uint32_t caps) {
    silk_dec_t *s = audf_calloc(1, sizeof(*s), caps);
    if (!s) return ESP_ERR_NO_MEM;
    s->channels = channels;
    silk_reset(s);
    *out = s;
    return ESP_OK;
}

void silk_destroy(silk_dec_t *s) {
    audf_free(s);
}
