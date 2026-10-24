/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "audf_opus.h"
#include "audf_opus_tables.h"
#include "esp_err.h"

/* Time and frequency samples are PCM x 2^SIG_SHIFT; unit-norm band shapes
 * are x 2^NORM_SHIFT. */
#define SIG_SHIFT  10
#define SIG_SAT    ((1 << 29) - 1)
#define NORM_SHIFT 20
#define NORM_ONE   (1 << NORM_SHIFT)

#define CELT_MAX_FRAME  960
#define CELT_MAX_PERIOD 1024
#define CELT_BUF        2048
#define CELT_MDCT_SIZES 4
#define CELT_MAX_PVQ_N  176

static inline int opus_ilog(uint32_t v) {
    return v ? 32 - __builtin_clz(v) : 0;
}

static inline int32_t celt_sat_sig(int64_t v) {
    return v > SIG_SAT ? SIG_SAT : v < -SIG_SAT ? -SIG_SAT : (int32_t)v;
}

static inline int32_t opus_mulh(int32_t a, int32_t b) {
    return (int32_t)(((int64_t)a * b) >> 32);
}

static inline int opus_imin(int a, int b) { return a < b ? a : b; }
static inline int opus_imax(int a, int b) { return a > b ? a : b; }

/* Plain float arithmetic only, so host and device round alike. */
float opus_exp2(float x);
/* cos(pi / 2 * x) for 0 <= x <= 1 */
float opus_cos_half_pi(float x);

typedef struct {
    const uint8_t *buf;
    uint32_t storage;
    uint32_t end_offs;
    uint32_t end_window;
    int      nend_bits;
    int      nbits_total;
    uint32_t offs;
    uint32_t rng;
    uint32_t val;
    uint32_t ext;
    int      rem;
    int      error;
} opus_rc_t;

void     opus_rc_init(opus_rc_t *rc, const uint8_t *buf, uint32_t len);
uint32_t opus_rc_decode(opus_rc_t *rc, uint32_t ft);
uint32_t opus_rc_decode_bin(opus_rc_t *rc, int bits);
void     opus_rc_update(opus_rc_t *rc, uint32_t fl, uint32_t fh, uint32_t ft);
int      opus_rc_bit_logp(opus_rc_t *rc, int logp);
int      opus_rc_icdf(opus_rc_t *rc, const uint8_t *icdf, int ftb);
uint32_t opus_rc_uint(opus_rc_t *rc, uint32_t ft);
uint32_t opus_rc_bits(opus_rc_t *rc, int bits);
uint32_t opus_rc_tell_frac(const opus_rc_t *rc);

static inline int opus_rc_tell(const opus_rc_t *rc) {
    return rc->nbits_total - opus_ilog(rc->rng);
}

/* Twiddles are Q31. */
typedef struct {
    uint16_t n;
    uint8_t  stages;
    uint8_t  radix[8];
    int32_t *tw;
    uint16_t *perm;
} opus_fft_t;

/* Index s is the transform of 1920 >> s samples. */
typedef struct {
    opus_fft_t fft[CELT_MDCT_SIZES];
    int32_t   *rot[CELT_MDCT_SIZES];
    int32_t   *buf;
} celt_mdct_t;

esp_err_t celt_mdct_init(celt_mdct_t *mdct, uint32_t caps);
void      celt_mdct_free(celt_mdct_t *mdct);
/* Inverse MDCT of 960 >> shift coefficients read with stride, windowed into
 * out[0 .. n + CELT_OVERLAP): the first CELT_OVERLAP samples are added to
 * what out holds, the rest overwritten. */
void      celt_imdct(const celt_mdct_t *mdct, const int32_t *in, int stride, int shift, int32_t *out);

/* V(n, k) of RFC 6716 4.3.4.2, the number of codewords of k pulses in n
 * dimensions, for every k a 32-bit codebook can have. */
typedef struct {
    uint32_t *v;
    uint16_t  row[CELT_MAX_PVQ_N + 1];
    uint8_t   max_k[CELT_MAX_PVQ_N + 1];
} celt_pvq_t;

esp_err_t celt_pvq_init(celt_pvq_t *pvq, uint32_t caps);

typedef struct {
    opus_rc_t *rc;
    const celt_pvq_t *pvq;
    int        lm;
    int        spread;
    int        intensity;
    int        stereo_inversion;
    int32_t    remaining_bits;
    uint32_t   seed;
    int32_t   *scratch;
    int32_t   *lowband_scratch;
    int       *iy;
    int        coded_bands;
} celt_bands_t;

void celt_quant_all_bands(celt_bands_t *ctx, int start, int end, int32_t *x, int32_t *y,
                          uint8_t *collapse, const int *pulses, int short_blocks,
                          int dual_stereo, const int *tf_res, int32_t total_bits,
                          int32_t balance, int32_t *norm);
void celt_anti_collapse(int32_t *x, const uint8_t *collapse, int lm, int c_count, int size,
                        int start, int end, const int32_t *log_e, const int32_t *prev1,
                        const int32_t *prev2, const int *pulses, uint32_t seed);
void celt_renormalise(int32_t *x, int n, float gain);
uint32_t celt_lcg(uint32_t seed);
int  celt_bits2pulses(int band, int lm, int bits);
int  celt_pulses2bits(int band, int lm, int pulses);

typedef struct {
    int pulses[CELT_BANDS];
    int fine[CELT_BANDS];
    int priority[CELT_BANDS];
    int intensity;
    int dual_stereo;
    int32_t balance;
    int coded_bands;
} celt_alloc_t;

/* What one frame works on, kept off the stack. */
typedef struct {
    int tf_res[CELT_BANDS];
    int cap[CELT_BANDS];
    int offsets[CELT_BANDS];
    int bits1[CELT_BANDS];
    int bits2[CELT_BANDS];
    int thresh[CELT_BANDS];
    int trim_offset[CELT_BANDS];
    uint8_t collapse[2 * CELT_BANDS];
    celt_alloc_t alloc;
    celt_bands_t bands;
} celt_frame_t;

typedef struct {
    uint8_t  channels;
    uint8_t  stream_channels;
    uint8_t  start;
    uint8_t  end;
    bool     stereo_inversion;
    uint32_t rng;
    int      loss_count;
    int      pf_period, pf_period_old;
    int32_t  pf_gain, pf_gain_old;
    int      pf_tapset, pf_tapset_old;
    int32_t  preemph_mem[2];
    int32_t  old_e[2 * CELT_BANDS];
    int32_t  old_loge[2 * CELT_BANDS];
    int32_t  old_loge2[2 * CELT_BANDS];
    int32_t  background_loge[2 * CELT_BANDS];
    int32_t *mem[2];
    int32_t *freq;
    int32_t *x;
    int32_t *norm;
    int32_t *scratch;
    int     *iy;
    celt_pvq_t pvq;
    celt_mdct_t mdct;
    celt_frame_t frame;
} celt_dec_t;

esp_err_t celt_init(celt_dec_t *st, int channels, uint32_t caps);
void      celt_free(celt_dec_t *st);
void      celt_reset(celt_dec_t *st);
/* Decodes n = 120 << lm samples per channel as PCM x 2^SIG_SHIFT into pcm,
 * interleaved; rc NULL conceals a lost frame. */
esp_err_t celt_decode(celt_dec_t *st, opus_rc_t *rc, uint32_t len, int lm, int32_t *pcm);

typedef struct silk_dec silk_dec_t;

esp_err_t silk_create(silk_dec_t **out, int channels, uint32_t caps);
void      silk_destroy(silk_dec_t *s);
void      silk_reset(silk_dec_t *s);
/* Decodes the next 10 or 20 ms SILK frame of a packet of payload_ms (the call
 * with first set also reads the packet's VAD and LBRR flags) and resamples it
 * to 48 kHz into out, interleaved; returns the samples per channel written.
 * lost conceals instead of reading rc. */
int       silk_decode(silk_dec_t *s, opus_rc_t *rc, bool lost, bool first, int internal_khz,
                      int stream_channels, int payload_ms, int16_t *out);
