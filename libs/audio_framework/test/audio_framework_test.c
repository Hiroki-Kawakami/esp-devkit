/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "audf_adpcm.h"
#include "audf_convert.h"
#include "audf_drift.h"
#include "audf_eq.h"
#include "audf_fifo.h"
#include "audf_gain.h"
#include "audf_graph.h"
#include "audf_mixer.h"
#include "audf_resampler.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

static int g_failures;

#define CHECK(cond, ...) do { \
    if (!(cond)) { \
        g_failures++; \
        printf("FAIL %s:%d: %s — ", __FILE__, __LINE__, #cond); \
        printf(__VA_ARGS__); \
        printf("\n"); \
    } \
} while (0)

static void sine16(int16_t *buf, size_t frames, uint8_t channels, double freq, double rate, double amp, double *phase) {
    for (size_t f = 0; f < frames; f++) {
        int16_t v = (int16_t)lrint(amp * sin(*phase));
        for (uint8_t ch = 0; ch < channels; ch++) buf[f * channels + ch] = v;
        *phase += 2.0 * M_PI * freq / rate;
    }
}

/* Amplitude of `freq` in a mono signal and the RMS of everything else, in dB below it. */
static double tone_purity_db(const int16_t *x, size_t n, size_t stride, double freq, double rate, double *amp) {
    double s = 0, c = 0;
    for (size_t i = 0; i < n; i++) {
        double w = 2.0 * M_PI * freq / rate * (double)i;
        s += x[i * stride] * sin(w);
        c += x[i * stride] * cos(w);
    }
    double a = 2.0 * sqrt(s * s + c * c) / (double)n;
    double ph = atan2(c, s);
    double err = 0;
    for (size_t i = 0; i < n; i++) {
        double ideal = a * sin(2.0 * M_PI * freq / rate * (double)i + ph);
        double d = x[i * stride] - ideal;
        err += d * d;
    }
    *amp = a;
    double rms = sqrt(err / (double)n);
    return 20.0 * log10((a / sqrt(2.0)) / (rms + 1e-9));
}

/* ---- convert ---- */

static void test_convert(void) {
    int16_t s16[4] = {32767, -32768, 1, -1};
    int32_t s32[4];
    audf_convert_from_pcm(s16, AUDF_PCM_S16, s32, AUDF_FMT_S32, 4);
    CHECK(s32[0] == 32767 << 8 && s32[1] == -32768 * 256, "s16 widens by 8 bits");

    uint8_t s24[12];
    audf_convert_to_pcm(s32, AUDF_FMT_S32, s24, AUDF_PCM_S24, 4);
    int32_t back[4];
    audf_convert_from_pcm(s24, AUDF_PCM_S24, back, AUDF_FMT_S32, 4);
    CHECK(memcmp(back, s32, sizeof(back)) == 0, "s24 round trip");

    int32_t loud[2] = {1 << 28, -(1 << 28)};
    int16_t out16[2];
    audf_convert(loud, AUDF_FMT_S32, out16, AUDF_FMT_S16, 2);
    CHECK(out16[0] == 32767 && out16[1] == -32768, "narrowing saturates (%d %d)", out16[0], out16[1]);
    uint8_t out32[8];
    audf_convert_to_pcm(loud, AUDF_FMT_S32, out32, AUDF_PCM_S32, 2);
    int32_t full = (int32_t)((uint32_t)out32[0] | (uint32_t)out32[1] << 8 | (uint32_t)out32[2] << 16 | (uint32_t)out32[3] << 24);
    CHECK(full == 0x7fffff00, "headroom clips at the 32-bit edge (%x)", full);

    int32_t inplace[4];
    int16_t *as16 = (int16_t *)inplace;
    as16[0] = 100; as16[1] = -100; as16[2] = 1000; as16[3] = -1000;
    audf_convert(inplace, AUDF_FMT_S16, inplace, AUDF_FMT_S32, 4);
    CHECK(inplace[0] == 100 << 8 && inplace[3] == -1000 * 256, "in-place widening");
    audf_convert(inplace, AUDF_FMT_S32, inplace, AUDF_FMT_S16, 4);
    CHECK(as16[0] == 100 && as16[1] == -100 && as16[2] == 1000 && as16[3] == -1000, "in-place narrowing");
}

/* ---- eq ---- */

static audf_eq_t *make_eq(audf_fmt_t fmt, uint8_t ch) {
    audf_eq_t *eq = NULL;
    audf_eq_create(&(audf_eq_config_t){ .fmt = fmt, .channels = ch, .enabled = true }, &eq);
    return eq;
}

static void test_eq(void) {
    const uint32_t fs = 48000;
    audf_eq_t *eq = make_eq(AUDF_FMT_S16, 1);
    int16_t buf[256], ref[256];
    for (int i = 0; i < 256; i++) buf[i] = ref[i] = (int16_t)(i * 257 - 32768);
    audf_eq_process(eq, buf, buf, 256);
    CHECK(memcmp(buf, ref, sizeof(buf)) == 0, "no stages is a bit-exact bypass");

    audf_biquad_t lp = audf_biquad_lowpass(fs, 1000.0f, 0.707f);
    audf_eq_set_biquads(eq, &lp, 1);
    enum { N = 2000 };
    static int16_t x[N];
    for (int i = 0; i < N; i++) x[i] = 8000;
    audf_eq_process(eq, x, x, N);
    CHECK(abs(x[N - 1] - 8000) < 20, "DC passes (%d)", x[N - 1]);
    for (int i = 0; i < N; i++) x[i] = (i & 1) ? -8000 : 8000;
    audf_eq_process(eq, x, x, N);
    int peak = 0;
    for (int i = N / 2; i < N; i++) peak = abs(x[i]) > peak ? abs(x[i]) : peak;
    CHECK(peak < 100, "Nyquist attenuated (%d)", peak);

    audf_eq_set_enabled(eq, false);
    int16_t raw[2] = {8000, -8000};
    audf_eq_process(eq, raw, raw, 2);
    CHECK(raw[0] == 8000 && raw[1] == -8000, "disabled is a bypass");
    CHECK(!audf_eq_get_enabled(eq), "get reflects disable");
    audf_eq_destroy(eq);

    audf_biquad_t shelf = audf_biquad_low_shelf(fs, 10000.0f, 0.707f, 12.0f);
    eq = make_eq(AUDF_FMT_S16, 1);
    audf_eq_set_biquads(eq, &shelf, 1);
    for (int i = 0; i < 512; i++) buf[i % 256] = (i / 64 % 2) ? -30000 : 30000;
    audf_eq_process(eq, buf, buf, 256);
    int boosted = 0;
    for (int i = 0; i < 256; i++) boosted |= abs(buf[i]) > 30000;
    CHECK(boosted, "shelf boosts and saturates instead of wrapping");
    audf_eq_destroy(eq);

    /* Re-sending the same coefficients mid-stream must not disturb the filter. */
    audf_biquad_t pk = audf_biquad_peaking(fs, 200.0f, 1.0f, 6.0f);
    audf_eq_t *a = make_eq(AUDF_FMT_S16, 2), *b = make_eq(AUDF_FMT_S16, 2);
    audf_eq_set_biquads(a, &pk, 1);
    audf_eq_set_biquads(b, &pk, 1);
    static int16_t sa[2 * 1000], sb[2 * 1000];
    double ph = 0;
    sine16(sa, 1000, 2, 300.0, fs, 12000, &ph);
    memcpy(sb, sa, sizeof(sa));
    audf_eq_process(a, sa, sa, 1000);
    audf_eq_process(b, sb, sb, 500);
    audf_eq_set_biquads(b, &pk, 1);
    audf_eq_process(b, sb + 1000, sb + 1000, 500);
    CHECK(memcmp(sa, sb, sizeof(sa)) == 0, "coefficient update keeps state");
    audf_eq_destroy(a);
    audf_eq_destroy(b);

    /* S32 carries the same response with more resolution. */
    audf_eq_t *e16 = make_eq(AUDF_FMT_S16, 1), *e32 = make_eq(AUDF_FMT_S32, 1);
    audf_eq_set_biquads(e16, &pk, 1);
    audf_eq_set_biquads(e32, &pk, 1);
    static int16_t m16[1000];
    static int32_t m32[1000];
    ph = 0;
    sine16(m16, 1000, 1, 300.0, fs, 8000, &ph);
    audf_convert(m16, AUDF_FMT_S16, m32, AUDF_FMT_S32, 1000);
    audf_eq_process(e16, m16, m16, 1000);
    audf_eq_process(e32, m32, m32, 1000);
    int worst = 0;
    for (int i = 0; i < 1000; i++) {
        int d = abs(m16[i] - (int)lrint(m32[i] / 256.0));
        worst = d > worst ? d : worst;
    }
    CHECK(worst <= 1, "S16 and S32 agree within an LSB (%d)", worst);
    audf_eq_destroy(e16);
    audf_eq_destroy(e32);
}

/* ---- gain ---- */

static audf_gain_t *make_gain(uint32_t rate, uint8_t ch, float g) {
    audf_gain_t *gain = NULL;
    audf_gain_create(&(audf_gain_config_t){ .fmt = AUDF_FMT_S16, .channels = ch, .sample_rate = rate, .gain = g }, &gain);
    return gain;
}

static void test_gain(void) {
    audf_gain_t *g = make_gain(1000, 1, 1.0f);
    int16_t buf[200], ref[200];
    for (int i = 0; i < 200; i++) buf[i] = ref[i] = (int16_t)(i * 100);
    audf_gain_process(g, buf, buf, 200);
    CHECK(memcmp(buf, ref, sizeof(buf)) == 0, "unity is a bypass");

    audf_gain_set(g, 0.0f, 0);
    audf_gain_set(g, 1.0f, 100);
    CHECK(fabsf(audf_gain_get(g) - 1.0f) < 1e-6f, "get returns the target");
    for (int i = 0; i < 200; i++) buf[i] = 10000;
    audf_gain_process(g, buf, buf, 200);
    CHECK(buf[0] < 500, "fade starts from the jump (%d)", buf[0]);
    for (int i = 1; i < 100; i++) CHECK(buf[i] >= buf[i - 1], "monotonic at %d", i);
    for (int i = 100; i < 200; i++) CHECK(buf[i] == 10000, "lands on target at %d (%d)", i, buf[i]);
    audf_gain_destroy(g);

    enum { N = 750 };
    int16_t one[N], chunked[N];
    for (int i = 0; i < N; i++) one[i] = chunked[i] = 12345;
    audf_gain_t *a = make_gain(1000, 1, 0.0f), *b = make_gain(1000, 1, 0.0f);
    audf_gain_set(a, 0.8f, 500);
    audf_gain_set(b, 0.8f, 500);
    audf_gain_process(a, one, one, N);
    const size_t chunks[] = {13, 1, 257, 64, 199, 216};
    size_t off = 0;
    for (size_t c = 0; c < sizeof(chunks) / sizeof(chunks[0]); c++) {
        audf_gain_process(b, chunked + off, chunked + off, chunks[c]);
        off += chunks[c];
    }
    CHECK(memcmp(one, chunked, sizeof(one)) == 0, "fade is chunk-invariant");
    audf_gain_destroy(a);
    audf_gain_destroy(b);

    g = make_gain(48000, 1, 2.0f);
    int16_t boost[4] = {1000, -1000, 30000, -30000};
    audf_gain_process(g, boost, boost, 4);
    CHECK(boost[0] == 2000 && boost[1] == -2000 && boost[2] == 32767 && boost[3] == -32768, "boost saturates");
    audf_gain_set(g, 1000.0f, 0);
    CHECK(audf_gain_get(g) == AUDF_GAIN_MAX, "clamps to max");
    audf_gain_set(g, -1.0f, 0);
    CHECK(audf_gain_get(g) == 0.0f, "clamps to zero");
    audf_gain_destroy(g);
}

/* ---- mixer ---- */

static void test_mixer(void) {
    audf_mixer_t *m = NULL;
    audf_mixer_create(&(audf_mixer_config_t){
        .fmt = AUDF_FMT_S16, .out_channels = 2, .num_inputs = 1, .in_channels = (const uint8_t[]){2},
    }, &m);
    int16_t buf[8] = {1000, -1000, 2000, 0, -500, -500, 32767, 32767};
    int16_t ref[8];
    memcpy(ref, buf, sizeof(buf));
    audf_mixer_process(m, (const void *const[]){buf}, buf, 4);
    CHECK(memcmp(buf, ref, sizeof(buf)) == 0, "identity is a bypass");
    audf_mixer_set_matrix(m, 0, (const float[]){0.5f, 0.5f, 0.5f, 0.5f});
    audf_mixer_process(m, (const void *const[]){buf}, buf, 4);
    CHECK(buf[0] == 0 && buf[1] == 0, "L+R cancel");
    CHECK(buf[2] == 1000 && buf[3] == 1000, "half sum");
    CHECK(buf[4] == -500 && buf[5] == -500, "equal channels");
    CHECK(buf[6] == 32767 && buf[7] == 32767, "full scale survives");
    float got[4];
    audf_mixer_get_matrix(m, 0, got);
    CHECK(got[1] == 0.5f, "get_matrix");
    audf_mixer_destroy(m);

    audf_mixer_create(&(audf_mixer_config_t){
        .fmt = AUDF_FMT_S16, .out_channels = 2, .num_inputs = 2, .in_channels = (const uint8_t[]){2, 1},
    }, &m);
    int16_t st[4] = {100, 200, 300, 400};
    int16_t mono[2] = {10, 20};
    int16_t out[4];
    audf_mixer_process(m, (const void *const[]){st, mono}, out, 2);
    CHECK(out[0] == 110 && out[1] == 210 && out[2] == 320 && out[3] == 420, "stereo + upmixed mono");
    audf_mixer_process(m, (const void *const[]){st, NULL}, out, 2);
    CHECK(out[0] == 100 && out[3] == 400, "NULL input is silence");
    int16_t hot[4] = {30000, 30000, 30000, 30000};
    int16_t hot_m[2] = {30000, 30000};
    audf_mixer_process(m, (const void *const[]){hot, hot_m}, out, 2);
    CHECK(out[0] == 32767, "sum saturates");
    audf_mixer_destroy(m);
}

/* ---- resampler ---- */

static audf_resampler_t *make_rs(audf_resampler_kind_t kind, uint32_t in, uint32_t out, uint8_t ch) {
    audf_resampler_t *r = NULL;
    esp_err_t err = audf_resampler_create(&(audf_resampler_config_t){
        .kind = kind, .fmt = AUDF_FMT_S16, .channels = ch, .in_rate = in, .out_rate = out,
    }, &r);
    if (err != ESP_OK) {
        printf("FATAL: resampler create %d\n", err);
        exit(1);
    }
    return r;
}

static size_t run_push(audf_resampler_t *r, const int16_t *in, size_t in_frames, uint8_t ch,
                       int16_t *out, size_t out_cap, size_t chunk) {
    size_t produced = 0;
    for (size_t off = 0; off < in_frames; off += chunk) {
        size_t n = in_frames - off < chunk ? in_frames - off : chunk;
        size_t got = audf_resampler_max_out(r, n);
        if (produced + got > out_cap) got = out_cap - produced;
        audf_resampler_process(r, in + off * ch, n, out + produced * ch, &got);
        produced += got;
    }
    return produced;
}

static void check_tone(const char *name, audf_resampler_kind_t kind, uint32_t in_rate, uint32_t out_rate,
                       double freq, double min_db) {
    enum { IN = 24000 };
    static int16_t in[IN * 2], out[IN * 8 * 2];
    double ph = 0;
    sine16(in, IN, 2, freq, in_rate, 16000, &ph);
    audf_resampler_t *r = make_rs(kind, in_rate, out_rate, 2);
    size_t n = run_push(r, in, IN, 2, out, IN * 8, 480);
    double expect = (double)IN * out_rate / in_rate;
    CHECK(fabs((double)n - expect) < 100, "%s: output length %zu vs %.0f", name, n, expect);
    size_t skip = 256, len = n - 512;
    double amp;
    double db = tone_purity_db(out + skip * 2, len, 2, freq, out_rate, &amp);
    CHECK(db > min_db, "%s: tone purity %.1f dB", name, db);
    CHECK(fabs(amp - 16000) < 400, "%s: amplitude %.0f", name, amp);
    double db_r = tone_purity_db(out + skip * 2 + 1, len, 2, freq, out_rate, &amp);
    CHECK(db_r > min_db, "%s: right channel %.1f dB", name, db_r);
    audf_resampler_destroy(r);
}

static void test_resampler(void) {
    check_tone("cubic 48k->48.048k", AUDF_RESAMPLER_CUBIC, 48000, 48048, 1000.0, 50.0);
    check_tone("polyphase 44.1k->48k", AUDF_RESAMPLER_POLYPHASE, 44100, 48000, 5000.0, 60.0);
    check_tone("polyphase 48k->16k", AUDF_RESAMPLER_POLYPHASE, 48000, 16000, 3000.0, 60.0);
    check_tone("integer 16k->48k", AUDF_RESAMPLER_INTEGER, 16000, 48000, 3000.0, 60.0);
    check_tone("integer 48k->24k", AUDF_RESAMPLER_INTEGER, 48000, 24000, 3000.0, 60.0);

    /* Pull: frames_needed must yield exactly the requested count every time. */
    audf_resampler_kind_t kinds[] = {AUDF_RESAMPLER_CUBIC, AUDF_RESAMPLER_POLYPHASE, AUDF_RESAMPLER_INTEGER};
    uint32_t outs[] = {48000, 48000, 32000};
    for (size_t k = 0; k < 3; k++) {
        audf_resampler_t *r = make_rs(kinds[k], k == 2 ? 16000 : 44100, outs[k], 1);
        static int16_t in[2048], out[1024];
        double ph = 0;
        bool exact = true;
        for (int it = 0; it < 200; it++) {
            size_t want = 100 + (size_t)(it * 37 % 300);
            if (kinds[k] != AUDF_RESAMPLER_INTEGER) audf_resampler_set_adjust(r, (float)(it % 7) * 50.0f - 150.0f);
            size_t need = audf_resampler_frames_needed(r, want);
            sine16(in, need, 1, 440, 44100, 10000, &ph);
            size_t got = want;
            audf_resampler_process(r, in, need, out, &got);
            if (got != want) exact = false;
        }
        CHECK(exact, "kind %zu: frames_needed yields exact output", k);
        audf_resampler_destroy(r);
    }

    /* Push in odd chunks equals push in one go. */
    static int16_t in[4000], o1[8000], o2[8000];
    double ph = 0;
    sine16(in, 4000, 1, 700, 44100, 12000, &ph);
    audf_resampler_t *a = make_rs(AUDF_RESAMPLER_POLYPHASE, 44100, 48000, 1);
    audf_resampler_t *b = make_rs(AUDF_RESAMPLER_POLYPHASE, 44100, 48000, 1);
    size_t na = run_push(a, in, 4000, 1, o1, 8000, 1000);
    size_t nb = run_push(b, in, 4000, 1, o2, 8000, 77);
    CHECK(na == nb && memcmp(o1, o2, na * 2) == 0, "chunk-invariant (%zu vs %zu)", na, nb);
    audf_resampler_destroy(a);
    audf_resampler_destroy(b);

    /* +1000 ppm consumes ~0.1% more input per output. */
    audf_resampler_t *r = make_rs(AUDF_RESAMPLER_CUBIC, 48000, 48000, 1);
    audf_resampler_set_adjust(r, 1000.0f);
    size_t total = 0;
    static int16_t tmp_in[1024], tmp_out[1024];
    memset(tmp_in, 0, sizeof(tmp_in));
    for (int i = 0; i < 100; i++) {
        size_t need = audf_resampler_frames_needed(r, 1000);
        size_t got = 1000;
        audf_resampler_process(r, tmp_in, need, tmp_out, &got);
        total += need;
    }
    CHECK(total > 100090 && total < 100110, "adjust shifts consumption (%zu)", total);
    CHECK(audf_resampler_set_adjust(make_rs(AUDF_RESAMPLER_INTEGER, 16000, 48000, 1), 1.0f) == ESP_ERR_NOT_SUPPORTED,
          "integer has no adjust");
    audf_resampler_destroy(r);
}

/* ---- fifo ---- */

static audf_fifo_t *make_fifo(size_t cap, size_t prefill, audf_fifo_underrun_t u, audf_fifo_overrun_t o) {
    audf_fifo_t *f = NULL;
    audf_fifo_create(&(audf_fifo_config_t){
        .fmt = AUDF_FMT_S16, .channels = 1, .capacity = cap, .prefill = prefill, .underrun = u, .overrun = o,
    }, &f);
    return f;
}

static void *abort_later(void *arg) {
    usleep(50 * 1000);
    audf_fifo_abort(arg);
    return NULL;
}

static void *write_later(void *arg) {
    usleep(50 * 1000);
    int16_t data[8] = {1, 2, 3, 4, 5, 6, 7, 8};
    audf_fifo_write(arg, data, 8);
    return NULL;
}

static void test_fifo(void) {
    audf_fifo_t *f = make_fifo(8, 4, AUDF_FIFO_UNDERRUN_SILENCE, AUDF_FIFO_OVERRUN_DROP_OLDEST);
    int16_t out[6];
    int16_t data[10] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10};
    audf_fifo_write(f, data, 3);
    audf_fifo_read(f, out, 2);
    CHECK(out[0] == 0 && out[1] == 0 && !audf_fifo_primed(f), "silence until prefill");
    audf_fifo_write(f, data + 3, 1);
    CHECK(audf_fifo_primed(f), "primed at prefill");
    audf_fifo_read(f, out, 6);
    CHECK(out[0] == 1 && out[3] == 4 && out[4] == 0 && out[5] == 0, "underrun pads with silence");
    CHECK(!audf_fifo_primed(f), "underrun re-arms the prefill");
    audf_fifo_write(f, data, 10);
    CHECK(audf_fifo_level(f) == 8, "drop-oldest keeps the newest");
    audf_fifo_read(f, out, 2);
    CHECK(out[0] == 3 && out[1] == 4, "oldest dropped (%d %d)", out[0], out[1]);
    audf_fifo_flush(f);
    CHECK(audf_fifo_level(f) == 0 && !audf_fifo_primed(f), "flush empties and re-arms");
    audf_fifo_destroy(f);

    f = make_fifo(4, 0, AUDF_FIFO_UNDERRUN_BLOCK, AUDF_FIFO_OVERRUN_BLOCK);
    pthread_t t;
    pthread_create(&t, NULL, write_later, f);
    int16_t got[8];
    CHECK(audf_fifo_read(f, got, 8) == ESP_OK, "blocking read");
    CHECK(got[0] == 1 && got[7] == 8, "blocking read waits for and passes through a larger write");
    pthread_join(t, NULL);

    pthread_create(&t, NULL, abort_later, f);
    CHECK(audf_fifo_read(f, got, 1) == ESP_ERR_INVALID_STATE, "abort wakes a blocked reader");
    pthread_join(t, NULL);
    audf_fifo_destroy(f);
}

/* ---- drift ---- */

static void test_drift(void) {
    /* Producer runs 150 ppm fast against the reader; the loop must absorb it. */
    const uint32_t rate = 48000;
    audf_fifo_t *fifo = make_fifo(4800, 960, AUDF_FIFO_UNDERRUN_SILENCE, AUDF_FIFO_OVERRUN_DROP_OLDEST);
    audf_resampler_t *rs = make_rs(AUDF_RESAMPLER_CUBIC, rate, rate, 1);
    audf_drift_t drift;
    audf_drift_init(&drift, &(audf_drift_config_t){ .sample_rate = rate, .target_frames = 960 });

    audf_graph_t *g = NULL;
    audf_graph_create(&(audf_graph_config_t){ .max_frames = 480 }, &g);
    audf_node_t *fn = audf_graph_add_fifo(g, NULL, fifo);
    audf_node_t *rn = audf_graph_add_resampler(g, fn, rs, &drift);
    CHECK(audf_graph_build(g) == ESP_OK, "drift graph builds");

    static int16_t block[480 * 2];
    memset(block, 0, sizeof(block));
    double produced = 0;
    size_t written = 0;
    size_t worst = 0;
    for (int i = 0; i < 12000; i++) {
        produced += 480.0 * 1.00015;
        size_t n = (size_t)produced - written;
        audf_fifo_write(fifo, block, n);
        written += n;
        if (i > 1000) {
            size_t lvl = audf_fifo_level(fifo);
            size_t d = lvl > 960 ? lvl - 960 : 960 - lvl;
            worst = d > worst ? d : worst;
        }
        audf_graph_read(g, rn, block, 480);
    }
    CHECK(fabsf(drift.ppm - 150.0f) < 15.0f, "drift settles on the clock offset (%.1f ppm)", drift.ppm);
    CHECK(worst < 100, "level holds near target (worst %zu frames off)", worst);

    audf_fifo_flush(fifo);
    audf_graph_read(g, rn, block, 480);
    CHECK(drift.ppm == 0.0f, "flush resets the drift");
    audf_graph_destroy(g);
    audf_resampler_destroy(rs);
    audf_fifo_destroy(fifo);
}

/* ---- adpcm ---- */

static void test_adpcm(void) {
    const audf_adpcm_config_t cfg = { .channels = 2, .block_align = 1024 };
    size_t spb = audf_adpcm_block_frames(&cfg);
    CHECK(spb == 1017, "WAV IMA block holds 1017 frames (%zu)", spb);
    audf_encoder_t *enc = NULL;
    audf_decoder_t *dec = NULL;
    CHECK(audf_adpcm_encoder_create(&cfg, &enc) == ESP_OK, "encoder");
    CHECK(audf_adpcm_decoder_create(&cfg, &dec) == ESP_OK, "decoder");
    CHECK(audf_encoder_frame_frames(enc) == spb && audf_decoder_max_frames(dec) == spb, "sizes agree");

    enum { BLOCKS = 4 };
    static int16_t pcm[1017 * 2 * BLOCKS], decoded[1017 * 2 * BLOCKS];
    double ph = 0;
    sine16(pcm, spb * BLOCKS, 2, 440, 44100, 12000, &ph);
    for (size_t i = 0; i < spb * BLOCKS; i++) pcm[i * 2 + 1] = (int16_t)(pcm[i * 2] / 2);
    uint8_t block[1024];
    size_t total = 0;
    for (int b = 0; b < BLOCKS; b++) {
        size_t len = 0;
        audf_encoder_encode(enc, pcm + b * spb * 2, spb, block, &len);
        CHECK(len == 1024, "full block (%zu)", len);
        size_t frames = 0;
        CHECK(audf_decoder_decode(dec, block, len, decoded + total * 2, &frames) == ESP_OK, "decode");
        total += frames;
    }
    CHECK(total == spb * BLOCKS, "decoded length");
    double amp;
    double db = tone_purity_db(decoded, total, 2, 440, 44100, &amp);
    CHECK(db > 30.0 && fabs(amp - 12000) < 300, "left survives (%.1f dB, %.0f)", db, amp);
    db = tone_purity_db(decoded + 1, total, 2, 440, 44100, &amp);
    CHECK(db > 25.0 && fabs(amp - 6000) < 200, "right is its own channel (%.1f dB, %.0f)", db, amp);

    size_t len = 0;
    audf_encoder_encode(enc, pcm, 20, block, &len);
    CHECK(len == 8 * 4, "short block rounds up to whole groups (%zu)", len);
    size_t frames = 0;
    audf_decoder_decode(dec, block, len, decoded, &frames);
    CHECK(frames == 25, "short block decodes padded (%zu)", frames);
    CHECK(abs(decoded[19 * 2] - pcm[19 * 2]) < 600, "last real frame survives");
    CHECK(audf_decoder_decode(dec, block, 13, decoded, &frames) == ESP_ERR_INVALID_SIZE, "ragged length rejected");
    audf_encoder_destroy(enc);
    audf_decoder_destroy(dec);
}

/* ---- graph ---- */

typedef struct {
    int16_t data[8192];
    size_t  frames;
    uint8_t channels;
} capture_t;

static esp_err_t capture_write(void *user, void *data, size_t frames) {
    capture_t *c = user;
    memcpy(c->data + c->frames * c->channels, data, frames * c->channels * sizeof(int16_t));
    c->frames += frames;
    return ESP_OK;
}

typedef struct {
    double phase;
    double freq;
    size_t remaining;
} tone_src_t;

static size_t tone_read(void *user, void *data, size_t frames) {
    tone_src_t *t = user;
    size_t n = frames < t->remaining ? frames : t->remaining;
    sine16(data, n, 1, t->freq, 48000, 8000, &t->phase);
    t->remaining -= n;
    return n;
}

static void test_graph_push_chain(void) {
    audf_eq_t *eq = make_eq(AUDF_FMT_S16, 2);
    audf_biquad_t pk = audf_biquad_peaking(48000, 1000.0f, 1.0f, 6.0f);
    audf_eq_set_biquads(eq, &pk, 1);
    audf_gain_t *gain = make_gain(48000, 2, 0.5f);

    audf_graph_t *g = NULL;
    audf_graph_create(&(audf_graph_config_t){ .max_frames = 256 }, &g);
    static capture_t cap = { .channels = 2 };
    audf_node_t *in = audf_graph_add_input(g, AUDF_FMT_S16, 2);
    audf_node_t *n = audf_graph_add_eq(g, in, eq);
    n = audf_graph_add_gain(g, n, gain);
    audf_graph_add_sink(g, n, capture_write, &cap);
    CHECK(audf_graph_build(g) == ESP_OK, "push chain builds");

    static int16_t src[2 * 1000], ref[2 * 1000];
    double ph = 0;
    sine16(src, 1000, 2, 800, 48000, 10000, &ph);
    memcpy(ref, src, sizeof(src));
    CHECK(audf_graph_write(g, in, src, 1000) == ESP_OK, "write");
    CHECK(memcmp(src, ref, sizeof(src)) == 0, "caller's buffer is untouched");

    audf_eq_t *eq2 = make_eq(AUDF_FMT_S16, 2);
    audf_eq_set_biquads(eq2, &pk, 1);
    audf_gain_t *gain2 = make_gain(48000, 2, 0.5f);
    audf_eq_process(eq2, ref, ref, 1000);
    audf_gain_process(gain2, ref, ref, 1000);
    CHECK(cap.frames == 1000 && memcmp(cap.data, ref, sizeof(ref)) == 0, "graph equals the modules run by hand");
    CHECK(audf_graph_read(g, n, ref, 10) == ESP_ERR_INVALID_ARG, "push node can't be read");

    audf_graph_destroy(g);
    audf_eq_destroy(eq);
    audf_eq_destroy(eq2);
    audf_gain_destroy(gain);
    audf_gain_destroy(gain2);
}

static void test_graph_pull_mix(void) {
    audf_mixer_t *mixer = NULL;
    audf_mixer_create(&(audf_mixer_config_t){
        .fmt = AUDF_FMT_S16, .out_channels = 1, .num_inputs = 2, .in_channels = (const uint8_t[]){1, 1},
    }, &mixer);
    tone_src_t a = { .freq = 1000, .remaining = 10000 };
    audf_fifo_t *fifo = make_fifo(1024, 0, AUDF_FIFO_UNDERRUN_SILENCE, AUDF_FIFO_OVERRUN_BLOCK);

    audf_graph_t *g = NULL;
    audf_graph_create(NULL, &g);
    audf_node_t *src = audf_graph_add_source(g, AUDF_FMT_S16, 1, tone_read, &a);
    audf_node_t *fn = audf_graph_add_fifo(g, NULL, fifo);
    audf_node_t *mix = audf_graph_add_mixer(g, (audf_node_t *const[]){src, fn}, mixer);
    CHECK(audf_graph_build(g) == ESP_OK, "pull mixer builds");

    int16_t ones[100];
    for (int i = 0; i < 100; i++) ones[i] = 1000;
    audf_fifo_write(fifo, ones, 100);
    static int16_t out[2000];
    CHECK(audf_graph_read(g, mix, out, 2000) == ESP_OK, "read through the mixer");
    tone_src_t ref = { .freq = 1000, .remaining = 10000 };
    static int16_t expect[2000];
    tone_read(&ref, expect, 2000);
    bool ok = true;
    for (int i = 0; i < 2000; i++) {
        int e = expect[i] + (i < 100 ? 1000 : 0);
        if (out[i] != e) ok = false;
    }
    CHECK(ok, "source + FIFO, silence after the FIFO runs dry");
    audf_graph_destroy(g);
    audf_mixer_destroy(mixer);
    audf_fifo_destroy(fifo);
}

static void test_graph_push_mixer(void) {
    /* BGM pushes and paces; the second input is pulled through FIFO + resampler. */
    audf_mixer_t *mixer = NULL;
    audf_mixer_create(&(audf_mixer_config_t){
        .fmt = AUDF_FMT_S16, .out_channels = 1, .num_inputs = 2, .in_channels = (const uint8_t[]){1, 1},
    }, &mixer);
    audf_fifo_t *fifo = make_fifo(4096, 480, AUDF_FIFO_UNDERRUN_SILENCE, AUDF_FIFO_OVERRUN_DROP_OLDEST);
    audf_resampler_t *rs = make_rs(AUDF_RESAMPLER_POLYPHASE, 16000, 48000, 1);
    audf_drift_t drift;
    audf_drift_init(&drift, &(audf_drift_config_t){ .sample_rate = 16000, .target_frames = 480 });

    audf_graph_t *g = NULL;
    audf_graph_create(&(audf_graph_config_t){ .max_frames = 480 }, &g);
    static capture_t cap = { .channels = 1 };
    audf_node_t *bgm = audf_graph_add_input(g, AUDF_FMT_S16, 1);
    audf_node_t *fn = audf_graph_add_fifo(g, NULL, fifo);
    audf_node_t *rn = audf_graph_add_resampler(g, fn, rs, &drift);
    audf_node_t *mix = audf_graph_add_mixer(g, (audf_node_t *const[]){bgm, rn}, mixer);
    audf_graph_add_sink(g, mix, capture_write, &cap);
    CHECK(audf_graph_build(g) == ESP_OK, "push mixer with a pulled input builds");

    static int16_t mic[160], music[480];
    memset(music, 0, sizeof(music));
    double mph = 0;
    for (int i = 0; i < 16; i++) {
        sine16(mic, 160, 1, 500, 16000, 4000, &mph);
        audf_fifo_write(fifo, mic, 160);
        audf_graph_write(g, bgm, music, 480);
    }
    CHECK(cap.frames == 16 * 480, "output paced by the pushed input (%zu)", cap.frames);
    double amp;
    double db = tone_purity_db(cap.data + 6 * 480, 10 * 480, 1, 500, 48000, &amp);
    CHECK(amp > 3500 && db > 30.0, "pulled input mixed after resampling (%.0f, %.1f dB)", amp, db);
    audf_graph_destroy(g);
    audf_mixer_destroy(mixer);
    audf_resampler_destroy(rs);
    audf_fifo_destroy(fifo);
}

static esp_err_t null_write(void *user, void *data, size_t frames) {
    (void)user; (void)data; (void)frames;
    return ESP_OK;
}

static void test_graph_rules(void) {
    audf_mixer_t *mixer = NULL;
    audf_mixer_create(&(audf_mixer_config_t){
        .fmt = AUDF_FMT_S16, .out_channels = 1, .num_inputs = 2, .in_channels = (const uint8_t[]){1, 1},
    }, &mixer);
    tone_src_t t = { .freq = 100, .remaining = 1000 };
    audf_fifo_t *fifo = make_fifo(64, 0, AUDF_FIFO_UNDERRUN_SILENCE, AUDF_FIFO_OVERRUN_BLOCK);
    audf_graph_t *g;

    audf_graph_create(NULL, &g);
    audf_node_t *a = audf_graph_add_input(g, AUDF_FMT_S16, 1);
    audf_node_t *b = audf_graph_add_input(g, AUDF_FMT_S16, 1);
    audf_graph_add_mixer(g, (audf_node_t *const[]){a, b}, mixer);
    CHECK(audf_graph_build(g) == ESP_ERR_INVALID_ARG, "two pushed inputs rejected");
    audf_graph_destroy(g);

    audf_graph_create(NULL, &g);
    audf_node_t *s = audf_graph_add_source(g, AUDF_FMT_S16, 1, tone_read, &t);
    audf_graph_add_sink(g, s, null_write, NULL);
    CHECK(audf_graph_build(g) == ESP_ERR_INVALID_ARG, "sink on a pull node rejected");
    audf_graph_destroy(g);

    audf_graph_create(NULL, &g);
    s = audf_graph_add_source(g, AUDF_FMT_S16, 1, tone_read, &t);
    audf_graph_add_mixer(g, (audf_node_t *const[]){s, s}, mixer);
    CHECK(audf_graph_build(g) == ESP_ERR_INVALID_ARG, "pull fan-out rejected");
    audf_graph_destroy(g);

    audf_graph_create(NULL, &g);
    s = audf_graph_add_source(g, AUDF_FMT_S16, 1, tone_read, &t);
    audf_graph_add_fifo(g, s, fifo);
    CHECK(audf_graph_build(g) == ESP_ERR_INVALID_ARG, "FIFO fed from a pull node rejected");
    audf_graph_destroy(g);

    audf_graph_create(NULL, &g);
    a = audf_graph_add_input(g, AUDF_FMT_S16, 2);
    audf_graph_add_mixer(g, (audf_node_t *const[]){a, a}, mixer);
    CHECK(audf_graph_build(g) == ESP_ERR_INVALID_ARG, "channel mismatch rejected");
    audf_graph_destroy(g);

    audf_resampler_t *rs = make_rs(AUDF_RESAMPLER_CUBIC, 48000, 48000, 1);
    audf_drift_t drift;
    audf_drift_init(&drift, &(audf_drift_config_t){ .sample_rate = 48000, .target_frames = 10 });
    audf_graph_create(NULL, &g);
    a = audf_graph_add_input(g, AUDF_FMT_S16, 1);
    audf_graph_add_resampler(g, a, rs, &drift);
    CHECK(audf_graph_build(g) == ESP_ERR_INVALID_ARG, "drift without a FIFO rejected");
    audf_graph_destroy(g);

    audf_graph_create(NULL, &g);
    a = audf_graph_add_input(g, AUDF_FMT_S16, 1);
    audf_graph_add_convert(g, a, AUDF_FMT_S32);
    CHECK(audf_graph_build(g) == ESP_OK, "convert builds");
    CHECK(audf_graph_add_input(g, AUDF_FMT_S16, 1) == NULL, "no adds after build");
    audf_graph_destroy(g);

    audf_resampler_destroy(rs);
    audf_mixer_destroy(mixer);
    audf_fifo_destroy(fifo);
}

int main(void) {
    test_convert();
    test_eq();
    test_gain();
    test_mixer();
    test_resampler();
    test_fifo();
    test_drift();
    test_adpcm();
    test_graph_push_chain();
    test_graph_pull_mix();
    test_graph_push_mixer();
    test_graph_rules();

    if (g_failures) {
        printf("%d FAILURE(S)\n", g_failures);
        return 1;
    }
    printf("all audio_framework tests passed\n");
    return 0;
}
