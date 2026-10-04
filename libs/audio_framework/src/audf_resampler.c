/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_resampler.h"
#include <math.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include "audf_internal.h"
#include "audf_sample.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846f
#endif

#define DEFAULT_MAX_IN  1024
#define DEFAULT_TAPS    48
#define DEFAULT_PHASES  64
#define MAX_PHASES      1024
#define COEFF_Q         30
#define COEFF_HALF      ((int64_t)1 << (COEFF_Q - 1))
#define MU_BITS         16
#define KAISER_BETA     7.0f
#define PASSBAND        0.88f

struct audf_resampler {
    audf_resampler_kind_t kind;
    audf_fmt_t fmt;
    uint8_t    channels;
    uint32_t   in_rate;
    uint32_t   out_rate;
    size_t     max_in;

    size_t   left;     /* history frames before pos */
    size_t   right;    /* lookahead frames after pos */
    int32_t *buf;
    size_t   cap;
    size_t   len;
    size_t   pos;
    uint32_t frac;     /* Q32 */

    uint64_t    nominal_step;   /* Q32 input frames per output frame */
    uint32_t    step_int;
    uint32_t    step_frac;
    atomic_int  adjust_ppb;
    int32_t     applied_ppb;
    bool        latched;

    /* POLYPHASE: (phases + 1) rows. INTEGER: up_l rows, one per output phase. */
    int32_t *taps;
    size_t   ntaps;
    uint32_t phase_bits;
    uint32_t up_l;
    uint32_t down_m;
    uint32_t phase;
};

static float bessel_i0(float x) {
    float sum = 1.0f, term = 1.0f, q = x * x * 0.25f;
    for (int k = 1; k < 32; k++) {
        term *= q / (float)(k * k);
        sum += term;
        if (term < sum * 1e-9f) break;
    }
    return sum;
}

/* fc is in cycles per input frame. */
static void design_row(int32_t *row, float *tmp, size_t ntaps, float shift, float fc) {
    float half = (float)ntaps * 0.5f;
    float inv_i0 = 1.0f / bessel_i0(KAISER_BETA);
    float sum = 0.0f;
    for (size_t j = 0; j < ntaps; j++) {
        float t = (float)j - (half - 1.0f) - shift;
        float x = 2.0f * fc * t;
        float sinc = fabsf(x) < 1e-6f ? 1.0f : sinf((float)M_PI * x) / ((float)M_PI * x);
        float r = t / half;
        float w = r * r < 1.0f ? bessel_i0(KAISER_BETA * sqrtf(1.0f - r * r)) * inv_i0 : 0.0f;
        tmp[j] = sinc * w;
        sum += tmp[j];
    }
    for (size_t j = 0; j < ntaps; j++) {
        row[j] = (int32_t)lrintf(tmp[j] / sum * (float)(1 << COEFF_Q));
    }
}

static void update_step(audf_resampler_t *r) {
    int32_t ppb = atomic_load(&r->adjust_ppb);
    uint64_t step = r->nominal_step;
    if (ppb) {
        int64_t delta = (int64_t)((float)step * ((float)ppb * 1e-9f));
        step = (uint64_t)((int64_t)step + delta);
    }
    r->applied_ppb = ppb;
    r->step_int = (uint32_t)(step >> 32);
    r->step_frac = (uint32_t)step;
}

static void refresh_step(audf_resampler_t *r) {
    if (r->latched) return;
    if (atomic_load(&r->adjust_ppb) != r->applied_ppb) update_step(r);
}

esp_err_t audf_resampler_create(const audf_resampler_config_t *config, audf_resampler_t **out) {
    if (!config || !out || !config->in_rate || !config->out_rate) return ESP_ERR_INVALID_ARG;
    if (config->channels < 1 || config->channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;

    audf_resampler_t *r = calloc(1, sizeof(*r));
    if (!r) return ESP_ERR_NO_MEM;
    r->kind = config->kind;
    r->fmt = config->fmt;
    r->channels = config->channels;
    r->in_rate = config->in_rate;
    r->out_rate = config->out_rate;
    r->max_in = config->max_in_frames ? config->max_in_frames : DEFAULT_MAX_IN;
    r->nominal_step = ((uint64_t)config->in_rate << 32) / config->out_rate;
    atomic_init(&r->adjust_ppb, 0);
    update_step(r);

    size_t taps = config->taps ? config->taps : DEFAULT_TAPS;
    taps = (taps + 1) & ~(size_t)1;
    esp_err_t err = ESP_OK;
    switch (config->kind) {
        case AUDF_RESAMPLER_CUBIC:
            r->left = 1;
            r->right = 2;
            break;
        case AUDF_RESAMPLER_POLYPHASE: {
            uint32_t phases = config->phases ? config->phases : DEFAULT_PHASES;
            if (phases > MAX_PHASES || (phases & (phases - 1))) {
                err = ESP_ERR_INVALID_ARG;
                break;
            }
            float ratio = (float)config->out_rate / (float)config->in_rate;
            size_t scale = ratio < 1.0f ? (size_t)ceilf(1.0f / ratio) : 1;
            float fc = 0.5f * PASSBAND * (ratio < 1.0f ? ratio : 1.0f);
            r->ntaps = taps * scale;
            r->phase_bits = 0;
            while ((1u << r->phase_bits) < phases) r->phase_bits++;
            r->taps = malloc((phases + 1) * r->ntaps * sizeof(int32_t));
            float *tmp = malloc(r->ntaps * sizeof(float));
            if (!r->taps || !tmp) {
                free(tmp);
                err = ESP_ERR_NO_MEM;
                break;
            }
            for (uint32_t k = 0; k <= phases; k++) {
                design_row(&r->taps[k * r->ntaps], tmp, r->ntaps, (float)k / (float)phases, fc);
            }
            free(tmp);
            r->left = r->ntaps / 2 - 1;
            r->right = r->ntaps / 2;
            break;
        }
        case AUDF_RESAMPLER_INTEGER: {
            uint32_t l = 1, m = 1;
            if (config->out_rate % config->in_rate == 0) {
                l = config->out_rate / config->in_rate;
            } else if (config->in_rate % config->out_rate == 0) {
                m = config->in_rate / config->out_rate;
            } else {
                err = ESP_ERR_INVALID_ARG;
                break;
            }
            r->up_l = l;
            r->down_m = m;
            r->ntaps = taps * m;
            r->taps = malloc(l * r->ntaps * sizeof(int32_t));
            float *tmp = malloc(r->ntaps * sizeof(float));
            if (!r->taps || !tmp) {
                free(tmp);
                err = ESP_ERR_NO_MEM;
                break;
            }
            for (uint32_t k = 0; k < l; k++) {
                design_row(&r->taps[k * r->ntaps], tmp, r->ntaps, (float)k / (float)l, 0.5f * PASSBAND / (float)m);
            }
            free(tmp);
            r->left = r->ntaps / 2 - 1;
            r->right = r->ntaps / 2;
            break;
        }
        default:
            err = ESP_ERR_INVALID_ARG;
            break;
    }
    if (err == ESP_OK) {
        r->cap = r->left + r->right + r->max_in + (size_t)r->step_int + 2;
        r->buf = malloc(r->cap * r->channels * sizeof(int32_t));
        if (!r->buf) err = ESP_ERR_NO_MEM;
    }
    if (err != ESP_OK) {
        audf_resampler_destroy(r);
        return err;
    }
    audf_resampler_reset(r);
    *out = r;
    return ESP_OK;
}

void audf_resampler_destroy(audf_resampler_t *r) {
    if (!r) return;
    free(r->taps);
    free(r->buf);
    free(r);
}

void audf_resampler_reset(audf_resampler_t *r) {
    memset(r->buf, 0, r->left * r->channels * sizeof(int32_t));
    r->len = r->left;
    r->pos = r->left;
    r->frac = 0;
    r->phase = 0;
    r->latched = false;
}

esp_err_t audf_resampler_set_adjust(audf_resampler_t *r, float ppm) {
    if (!r) return ESP_ERR_INVALID_ARG;
    if (r->kind == AUDF_RESAMPLER_INTEGER) return ESP_ERR_NOT_SUPPORTED;
    if (ppm > AUDF_RESAMPLER_MAX_ADJUST_PPM) ppm = AUDF_RESAMPLER_MAX_ADJUST_PPM;
    if (ppm < -AUDF_RESAMPLER_MAX_ADJUST_PPM) ppm = -AUDF_RESAMPLER_MAX_ADJUST_PPM;
    atomic_store(&r->adjust_ppb, (int)lrintf(ppm * 1000.0f));
    return ESP_OK;
}

audf_fmt_t audf_resampler_fmt(const audf_resampler_t *r) { return r->fmt; }
uint8_t audf_resampler_channels(const audf_resampler_t *r) { return r->channels; }
size_t audf_resampler_max_in_frames(const audf_resampler_t *r) { return r->max_in; }

static size_t last_index(const audf_resampler_t *r, size_t out_frames) {
    size_t k = out_frames - 1;
    if (r->kind == AUDF_RESAMPLER_INTEGER) {
        return r->pos + (r->phase + k * r->down_m) / r->up_l;
    }
    uint64_t step = (uint64_t)r->step_int << 32 | r->step_frac;
    return r->pos + (size_t)(((uint64_t)r->frac + (uint64_t)k * step) >> 32);
}

size_t audf_resampler_frames_needed(audf_resampler_t *r, size_t out_frames) {
    if (!out_frames) return 0;
    r->latched = false;
    refresh_step(r);
    r->latched = true;
    size_t required = last_index(r, out_frames) + r->right + 1;
    return required > r->len ? required - r->len : 0;
}

size_t audf_resampler_max_out(const audf_resampler_t *r, size_t in_frames) {
    float ratio = (float)r->out_rate / (float)r->in_rate * (1.0f + AUDF_RESAMPLER_MAX_ADJUST_PPM * 1e-6f);
    return (size_t)ceilf((float)(in_frames + r->left + r->right + 2) * ratio) + 2;
}

size_t audf_resampler_max_in(const audf_resampler_t *r, size_t out_frames) {
    float ratio = (float)r->in_rate / (float)r->out_rate * (1.0f + AUDF_RESAMPLER_MAX_ADJUST_PPM * 1e-6f);
    return (size_t)ceilf((float)out_frames * ratio) + r->right + 2;
}

AUDF_INLINE int64_t dot(const int32_t *x, size_t stride, const int32_t *h, size_t n) {
    int64_t acc = 0;
    for (size_t j = 0; j < n; j++) acc += (int64_t)x[j * stride] * h[j];
    return acc;
}

AUDF_INLINE int64_t cubic(const int32_t *x, size_t stride, uint32_t frac) {
    int64_t xm1 = x[0], x0 = x[stride], x1 = x[2 * stride], x2 = x[3 * stride];
    int64_t t = frac >> 16;
    int64_t a = -xm1 + 3 * x0 - 3 * x1 + x2;
    int64_t b = 2 * xm1 - 5 * x0 + 4 * x1 - x2;
    int64_t c = x1 - xm1;
    int64_t v = a;
    v = ((v * t) >> 16) + b;
    v = ((v * t) >> 16) + c;
    v = ((v * t) >> 16) + 2 * x0;
    return v >> 1;
}

AUDF_INLINE size_t generate(audf_resampler_t *r, audf_fmt_t fmt, void *out, size_t produced, size_t limit) {
    const size_t channels = r->channels;
    while (produced < limit && r->pos + r->right < r->len) {
        const int32_t *base = &r->buf[(r->pos - r->left) * channels];
        for (size_t ch = 0; ch < channels; ch++) {
            int64_t y;
            if (r->kind == AUDF_RESAMPLER_CUBIC) {
                y = cubic(base + ch, channels, r->frac);
            } else if (r->kind == AUDF_RESAMPLER_POLYPHASE) {
                uint32_t k = r->phase_bits ? r->frac >> (32 - r->phase_bits) : 0;
                int64_t mu = (uint32_t)(r->frac << r->phase_bits) >> (32 - MU_BITS);
                const int32_t *h0 = &r->taps[k * r->ntaps];
                int64_t y0 = (dot(base + ch, channels, h0, r->ntaps) + COEFF_HALF) >> COEFF_Q;
                int64_t y1 = (dot(base + ch, channels, h0 + r->ntaps, r->ntaps) + COEFF_HALF) >> COEFF_Q;
                y = y0 + (((y1 - y0) * mu) >> MU_BITS);
            } else {
                const int32_t *h = &r->taps[r->phase * r->ntaps];
                y = (dot(base + ch, channels, h, r->ntaps) + COEFF_HALF) >> COEFF_Q;
            }
            audf_store(fmt, out, produced * channels + ch, y);
        }
        produced++;
        if (r->kind == AUDF_RESAMPLER_INTEGER) {
            r->phase += r->down_m;
            r->pos += r->phase / r->up_l;
            r->phase %= r->up_l;
        } else {
            uint32_t frac = r->frac + r->step_frac;
            r->pos += r->step_int + (frac < r->frac);
            r->frac = frac;
        }
    }
    return produced;
}

static void compact(audf_resampler_t *r) {
    size_t drop = r->pos - r->left;
    if (drop > r->len) drop = r->len;
    if (!drop) return;
    memmove(r->buf, &r->buf[drop * r->channels], (r->len - drop) * r->channels * sizeof(int32_t));
    r->len -= drop;
    r->pos -= drop;
}

AUDF_INLINE esp_err_t run(audf_resampler_t *r, audf_fmt_t fmt, const void *in, size_t in_frames,
                          void *out, size_t *out_frames) {
    const size_t channels = r->channels;
    const size_t limit = *out_frames;
    size_t produced = 0, consumed = 0;
    for (;;) {
        size_t space = r->cap - r->len;
        size_t n = in_frames - consumed;
        if (n > space) n = space;
        int32_t *dst = &r->buf[r->len * channels];
        size_t base = consumed * channels;
        for (size_t i = 0; i < n * channels; i++) dst[i] = audf_load(fmt, in, base + i);
        r->len += n;
        consumed += n;
        produced = generate(r, fmt, out, produced, limit);
        compact(r);
        if (consumed == in_frames) break;
        if (r->len == r->cap) {
            *out_frames = produced;
            return ESP_ERR_INVALID_SIZE;
        }
    }
    *out_frames = produced;
    return ESP_OK;
}

esp_err_t audf_resampler_process(audf_resampler_t *r, const void *in, size_t in_frames,
                                 void *out, size_t *out_frames) {
    if (!r || !out_frames || (in_frames && !in)) return ESP_ERR_INVALID_ARG;
    if (in_frames > r->max_in) return ESP_ERR_INVALID_SIZE;
    refresh_step(r);
    r->latched = false;
    if (r->fmt == AUDF_FMT_S16) return run(r, AUDF_FMT_S16, in, in_frames, out, out_frames);
    return run(r, AUDF_FMT_S32, in, in_frames, out, out_frames);
}
