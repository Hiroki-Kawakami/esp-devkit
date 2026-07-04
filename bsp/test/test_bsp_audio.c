/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host test for the bsp_audio dispatch policy, against stub providers (no
 * SDL, no hardware): DSP voicing modes (Auto applies the board profile at
 * boot/open and re-voices on headphone insert/remove via the route task;
 * Manual stays flat; Disable has no DSP), amp arming (the speaker gate stays
 * off until the first open, then follows ON/AUTO/OFF + HP state), the
 * headphone insert callback, idempotent open/close (same format = no-op,
 * different format = provider re-open, close-while-closed = no-op), and the
 * tone synth fallback (CAP_PCM without CAP_TONE) vs. direct CAP_TONE
 * forwarding.
 */

#include "bsp.h"
#include "bsp_audio.h"
#include <math.h>
#include <stdio.h>
#include <unistd.h>

static int g_failures;

#define CHECK(cond, ...) do { \
    if (!(cond)) { \
        g_failures++; \
        printf("FAIL %s:%d: %s — ", __FILE__, __LINE__, #cond); \
        printf(__VA_ARGS__); \
        printf("\n"); \
    } \
} while (0)

/* ---- full-caps stub: records every hardware op (Auto/Manual/Disable + idempotent open) ---- */

typedef struct {
    bsp_audio_t base;
    volatile bool hp;               /* simulated jack state */
    volatile bool speaker_enabled;  /* last amp gate write */
    volatile bool hw_muted;
    volatile int  hw_volume;
    volatile bool open;
    volatile int  open_count;
    volatile int  close_count;
} stub_t;

static esp_err_t st_open(bsp_audio_t *self, uint32_t r, uint8_t b, uint8_t c) {
    (void)r; (void)b; (void)c;
    stub_t *s = (stub_t *)self;
    s->open = true;
    s->open_count++;
    return ESP_OK;
}
static esp_err_t st_close(bsp_audio_t *self) {
    stub_t *s = (stub_t *)self;
    s->open = false;
    s->close_count++;
    return ESP_OK;
}
static esp_err_t st_write(bsp_audio_t *self, const void *d, size_t l) {
    (void)self; (void)d; (void)l;
    return ESP_OK;
}
static esp_err_t st_set_hw_volume(bsp_audio_t *self, int v) { ((stub_t *)self)->hw_volume = v; return ESP_OK; }
static esp_err_t st_set_hw_mute(bsp_audio_t *self, bool m) { ((stub_t *)self)->hw_muted = m; return ESP_OK; }
static esp_err_t st_set_speaker_enabled(bsp_audio_t *self, bool e) { ((stub_t *)self)->speaker_enabled = e; return ESP_OK; }
static esp_err_t st_headphone_inserted(bsp_audio_t *self, bool *out) { *out = ((stub_t *)self)->hp; return ESP_OK; }

/* Board voicing: speaker = 1 boosted stage + monomix; HP = flat stereo. */
static esp_err_t st_get_dsp_profile(bsp_audio_t *self, bool headphone, uint32_t rate,
                                    bsp_audio_dsp_profile_t *out) {
    (void)self;
    if (headphone) {
        out->num_stages = 0;
        out->eq_enabled = false;
        out->mono_mix   = false;
    } else {
        out->biquads[0] = audio_dsp_design_peaking(rate ? rate : 48000, 150.0f, 1.2f, 3.0f);
        out->num_stages = 1;
        out->eq_enabled = true;
        out->mono_mix   = true;
    }
    return ESP_OK;
}

static stub_t s_stub;

static bsp_audio_t *make_stub(void) {
    stub_t *s = &s_stub;
    *s = (stub_t){0};
    s->base.caps = BSP_AUDIO_CAP_PCM | BSP_AUDIO_CAP_SPEAKER | BSP_AUDIO_CAP_HEADPHONE;
    s->base.open                = st_open;
    s->base.close               = st_close;
    s->base.write               = st_write;
    s->base.set_hw_volume       = st_set_hw_volume;
    s->base.set_hw_mute         = st_set_hw_mute;
    s->base.set_speaker_enabled = st_set_speaker_enabled;
    s->base.headphone_inserted  = st_headphone_inserted;
    s->base.get_dsp_profile     = st_get_dsp_profile;
    return &s->base;
}

/* The route task polls at 200 ms; give it time to react. */
static void route_settle(void) { usleep(600 * 1000); }

static volatile int g_cb_count;
static volatile bool g_cb_last;
static void hp_cb(bool inserted, void *user) {
    (void)user;
    g_cb_last = inserted;
    g_cb_count++;
}

static void test_auto_mode(void) {
    bsp_audio_set_active(make_stub(), NULL);   /* zero-init: Auto + speaker ON */

    audio_dsp_t dsp = bsp_audio_dsp();
    CHECK(dsp != NULL, "Auto: DSP exists from boot");
    CHECK(audio_dsp_get_mono_mix(dsp), "Auto: speaker profile applied at boot");
    CHECK(audio_dsp_is_eq_enabled(dsp), "Auto: speaker EQ on");

    /* Amp stays off until the first open, even with speaker mode ON. */
    CHECK(!s_stub.speaker_enabled, "amp off before first open");
    int16_t buf[32] = {0};
    CHECK(bsp_audio_write(buf, sizeof(buf)) == ESP_ERR_INVALID_STATE, "write before open");

    CHECK(bsp_audio_open(44100, 16, 2) == ESP_OK, "open");
    CHECK(s_stub.speaker_enabled, "amp armed by first open");
    CHECK(s_stub.hw_volume == 100, "codec pinned to max (SW gain owns volume)");
    CHECK(bsp_audio_write(buf, sizeof(buf)) == ESP_OK, "write");

    /* HP insert: Auto re-voices the DSP and (speaker AUTO) drops the amp. */
    CHECK(bsp_audio_set_speaker_mode(BSP_AUDIO_SPEAKER_MODE_AUTO) == ESP_OK, "speaker AUTO");
    CHECK(bsp_audio_set_headphone_callback(hp_cb, NULL) == ESP_OK, "register cb");
    route_settle();
    s_stub.hp = true;
    route_settle();
    CHECK(!audio_dsp_get_mono_mix(dsp), "HP in: monomix off");
    CHECK(!audio_dsp_is_eq_enabled(dsp), "HP in: speaker EQ off");
    CHECK(!s_stub.speaker_enabled, "HP in: amp off (speaker AUTO)");
    CHECK(g_cb_count == 1 && g_cb_last, "insert callback fired");

    s_stub.hp = false;
    route_settle();
    CHECK(audio_dsp_get_mono_mix(dsp), "HP out: speaker profile back");
    CHECK(s_stub.speaker_enabled, "HP out: amp back on");
    CHECK(g_cb_count == 2 && !g_cb_last, "remove callback fired");
    bsp_audio_set_headphone_callback(NULL, NULL);

    /* close keeps the amp (per route policy) but hw-mutes the DAC first. */
    CHECK(bsp_audio_close() == ESP_OK, "close");
    CHECK(s_stub.hw_muted, "close hw-muted the DAC");
    CHECK(s_stub.speaker_enabled, "close keeps the amp");

    bsp_audio_quiesce();
    CHECK(!s_stub.speaker_enabled, "quiesce drops the amp");
}

static void test_eq_override(void) {
    /* The app EQ override must survive route re-voicing (the stub profile voices
     * EQ on for the speaker, off for the headphone). */
    bsp_audio_set_active(make_stub(), &(bsp_audio_init_t){
        .dsp_mode = BSP_AUDIO_DSP_MODE_AUTO,
        .speaker_mode = BSP_AUDIO_SPEAKER_MODE_AUTO,
    });
    audio_dsp_t dsp = bsp_audio_dsp();
    CHECK(bsp_audio_open(44100, 16, 2) == ESP_OK, "open");
    CHECK(audio_dsp_is_eq_enabled(dsp), "boot: speaker profile EQ on");

    /* Force EQ on, then plug HP (profile would voice it off): override wins. */
    CHECK(bsp_audio_set_eq_enabled(true) == ESP_OK, "force EQ on");
    s_stub.hp = true;
    route_settle();
    CHECK(audio_dsp_is_eq_enabled(dsp), "HP in: forced EQ on survives re-voice");
    CHECK(bsp_audio_get_eq_enabled(), "get reflects EQ on");

    /* Force EQ off, then unplug (profile would voice it on): override wins. */
    CHECK(bsp_audio_set_eq_enabled(false) == ESP_OK, "force EQ off");
    s_stub.hp = false;
    route_settle();
    CHECK(!audio_dsp_is_eq_enabled(dsp), "HP out: forced EQ off survives re-voice");
    CHECK(!bsp_audio_get_eq_enabled(), "get reflects EQ off");

    bsp_audio_close();
}

static void test_manual_mode(void) {
    bsp_audio_set_active(make_stub(), &(bsp_audio_init_t){
        .dsp_mode = BSP_AUDIO_DSP_MODE_MANUAL,
        .speaker_mode = BSP_AUDIO_SPEAKER_MODE_AUTO,
    });
    audio_dsp_t dsp = bsp_audio_dsp();
    CHECK(dsp != NULL, "Manual: DSP exists");
    CHECK(!audio_dsp_get_mono_mix(dsp), "Manual: flat init (no board profile)");
    CHECK(!audio_dsp_is_eq_enabled(dsp), "Manual: EQ off");

    CHECK(bsp_audio_open(48000, 16, 2) == ESP_OK, "open");

    /* SW gain curve: vol 100 = unity, the 100..150 region amplifies above unity
     * (vol 150 → +6 dB ≈ 2x). get_gain returns the (non-interpolated) target. */
    bsp_audio_set_volume(100);
    CHECK(fabsf(audio_dsp_get_gain(dsp) - 1.0f) < 0.01f, "vol 100 = unity gain");
    bsp_audio_set_volume(150);
    CHECK(audio_dsp_get_gain(dsp) > 1.9f && audio_dsp_get_gain(dsp) < 2.05f,
          "vol 150 boosts ~+6 dB above unity");
    bsp_audio_set_volume(100);

    audio_dsp_set_mono_mix(dsp, true);   /* app's own setting... */
    s_stub.hp = true;
    route_settle();                       /* ...survives an HP flip */
    CHECK(audio_dsp_get_mono_mix(dsp), "Manual: route change doesn't re-voice");
    CHECK(!s_stub.speaker_enabled, "speaker AUTO still routes the amp");
    bsp_audio_close();
}

static void test_disable_mode(void) {
    bsp_audio_set_active(make_stub(), &(bsp_audio_init_t){
        .dsp_mode = BSP_AUDIO_DSP_MODE_DISABLE,
    });
    CHECK(bsp_audio_dsp() == NULL, "Disable: no DSP");
    CHECK(bsp_audio_set_volume(40) == ESP_OK, "volume stored pre-open");
    CHECK(bsp_audio_open(48000, 16, 2) == ESP_OK, "open");
    CHECK(s_stub.hw_volume == 40, "no DSP: user volume lands on the codec");
    CHECK(bsp_audio_set_volume(55) == ESP_OK, "hw volume path");
    CHECK(s_stub.hw_volume == 55, "hw volume applied");
    CHECK(bsp_audio_set_volume(150) == ESP_OK, "boost value accepted");
    CHECK(s_stub.hw_volume == 100, "codec can't amplify: >100 clamped to 100");
    bsp_audio_close();
}

static void test_idempotent_open_close(void) {
    bsp_audio_set_active(make_stub(), NULL);

    CHECK(bsp_audio_open(44100, 16, 2) == ESP_OK, "first open");
    CHECK(s_stub.open_count == 1, "provider open called once");

    CHECK(bsp_audio_open(44100, 16, 2) == ESP_OK, "same-format open is a no-op");
    CHECK(s_stub.open_count == 1, "same format: provider open not called again");

    CHECK(bsp_audio_open(22050, 16, 1) == ESP_OK, "format-change open");
    CHECK(s_stub.open_count == 2, "different format: provider open called again");

    CHECK(bsp_audio_close() == ESP_OK, "close");
    CHECK(s_stub.close_count == 1, "provider close called once");
    CHECK(bsp_audio_close() == ESP_OK, "close on a closed stream is a no-op");
    CHECK(s_stub.close_count == 1, "no-op close doesn't call the provider again");
}

/* ---- PCM-only stub (no TONE, no speaker/headphone): the tone synth fallback ---- */

typedef struct {
    bsp_audio_t base;
    volatile bool open;
    volatile int  open_count;
    volatile int  close_count;
    volatile int  peak;  /* max abs sample seen across writes since the last open */
} pcm_stub_t;

static esp_err_t pcm_open(bsp_audio_t *self, uint32_t r, uint8_t b, uint8_t c) {
    (void)r; (void)b; (void)c;
    pcm_stub_t *s = (pcm_stub_t *)self;
    s->open = true;
    s->open_count++;
    s->peak = 0;
    return ESP_OK;
}
static esp_err_t pcm_close(bsp_audio_t *self) {
    pcm_stub_t *s = (pcm_stub_t *)self;
    s->open = false;
    s->close_count++;
    return ESP_OK;
}
static esp_err_t pcm_write(bsp_audio_t *self, const void *data, size_t len) {
    pcm_stub_t *s = (pcm_stub_t *)self;
    const int16_t *p = data;
    for (size_t i = 0; i < len / sizeof(int16_t); i++) {
        int v = p[i] < 0 ? -(int)p[i] : (int)p[i];
        if (v > s->peak) s->peak = v;
    }
    return ESP_OK;
}

static pcm_stub_t s_pcm_stub;

static bsp_audio_t *make_pcm_stub(void) {
    pcm_stub_t *s = &s_pcm_stub;
    *s = (pcm_stub_t){0};
    s->base.caps  = BSP_AUDIO_CAP_PCM;
    s->base.open  = pcm_open;
    s->base.close = pcm_close;
    s->base.write = pcm_write;
    return &s->base;
}

/* Ensure any tone task activity on s_pcm_stub has settled before the next
 * make_pcm_stub() zeroes it out from under a still-running task. */
static void tone_settle(void) {
    bsp_audio_tone_stop();
    usleep(200 * 1000);
}

static void test_tone_fallback_default_audibility(void) {
    bsp_audio_set_active(make_pcm_stub(), NULL);  /* volume never set (s_volume == -1) */
    CHECK(bsp_audio_get_caps() == BSP_AUDIO_CAP_PCM, "pcm-only caps");

    CHECK(bsp_audio_tone(440, 50) == ESP_OK, "tone starts");
    usleep(250 * 1000);
    CHECK(s_pcm_stub.open_count == 1, "tone opened the provider stream");
    CHECK(s_pcm_stub.peak > 1000, "audible by default with no volume ever set (peak %d)", s_pcm_stub.peak);
    CHECK(!s_pcm_stub.open, "tone closed the stream after its duration");
    CHECK(s_pcm_stub.close_count == 1, "provider close called");
}

static void test_tone_stop(void) {
    bsp_audio_set_active(make_pcm_stub(), NULL);
    CHECK(bsp_audio_tone(440, 0) == ESP_OK, "tone starts (until stop)");
    usleep(50 * 1000);
    CHECK(s_pcm_stub.open, "tone stream open while playing");
    CHECK(bsp_audio_tone_stop() == ESP_OK, "tone_stop");
    usleep(150 * 1000);
    CHECK(!s_pcm_stub.open, "tone stream closed after stop");
}

static void test_tone_invalid_state_while_app_open(void) {
    bsp_audio_set_active(make_pcm_stub(), NULL);
    CHECK(bsp_audio_open(44100, 16, 2) == ESP_OK, "app opens the stream");
    CHECK(bsp_audio_tone(440, 50) == ESP_ERR_INVALID_STATE, "tone rejected while app stream open");
    CHECK(bsp_audio_close() == ESP_OK, "app closes the stream");
}

static void test_tone_respects_volume_and_mute(void) {
    bsp_audio_set_active(make_pcm_stub(), NULL);

    CHECK(bsp_audio_set_volume(100) == ESP_OK, "volume set to 100");
    CHECK(bsp_audio_tone(440, 50) == ESP_OK, "tone starts");
    usleep(250 * 1000);
    CHECK(s_pcm_stub.peak > 20000, "vol 100 tone near full scale (peak %d)", s_pcm_stub.peak);
    tone_settle();

    CHECK(bsp_audio_set_mute(true) == ESP_OK, "mute");
    CHECK(bsp_audio_tone(440, 50) == ESP_OK, "tone starts muted");
    usleep(250 * 1000);
    CHECK(s_pcm_stub.peak == 0, "muted tone is silent (peak %d)", s_pcm_stub.peak);
    bsp_audio_set_mute(false);
}

/* ---- CAP_TONE stub: direct hardware forwarding, no synth involved ---- */

typedef struct {
    bsp_audio_t base;
    volatile int      tone_count;
    volatile uint32_t tone_freq;
    volatile uint32_t tone_duration;
    volatile int      tone_stop_count;
} tone_stub_t;

static esp_err_t tn_tone(bsp_audio_t *self, uint32_t freq_hz, uint32_t duration_ms) {
    tone_stub_t *s = (tone_stub_t *)self;
    s->tone_count++;
    s->tone_freq = freq_hz;
    s->tone_duration = duration_ms;
    return ESP_OK;
}
static esp_err_t tn_tone_stop(bsp_audio_t *self) {
    ((tone_stub_t *)self)->tone_stop_count++;
    return ESP_OK;
}

static tone_stub_t s_tone_stub;

static bsp_audio_t *make_tone_stub(void) {
    tone_stub_t *s = &s_tone_stub;
    *s = (tone_stub_t){0};
    s->base.caps      = BSP_AUDIO_CAP_TONE;
    s->base.tone      = tn_tone;
    s->base.tone_stop = tn_tone_stop;
    return &s->base;
}

static void test_tone_cap_tone_forwarding(void) {
    bsp_audio_set_active(make_tone_stub(), NULL);
    CHECK(bsp_audio_get_caps() == BSP_AUDIO_CAP_TONE, "tone-only caps");

    CHECK(bsp_audio_tone(880, 200) == ESP_OK, "tone forwarded");
    CHECK(s_tone_stub.tone_count == 1 && s_tone_stub.tone_freq == 880 &&
          s_tone_stub.tone_duration == 200, "provider tone() received the args");

    CHECK(bsp_audio_tone_stop() == ESP_OK, "tone_stop forwarded");
    CHECK(s_tone_stub.tone_stop_count == 1, "provider tone_stop() called");
}

int main(void) {
    test_auto_mode();
    test_eq_override();
    test_manual_mode();
    test_disable_mode();
    test_idempotent_open_close();
    test_tone_fallback_default_audibility();
    test_tone_stop();
    test_tone_invalid_state_while_app_open();
    test_tone_respects_volume_and_mute();
    tone_settle();
    test_tone_cap_tone_forwarding();
    bsp_audio_set_active(NULL, NULL);

    if (g_failures) {
        printf("%d FAILURE(S)\n", g_failures);
        return 1;
    }
    printf("all bsp_audio tests passed\n");
    return 0;
}
