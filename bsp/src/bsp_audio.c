/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic audio layer: holds the active bsp_audio provider (registered
 * by the board's bsp_init via bsp_audio_set_active) and implements the public
 * bsp_audio_* API by dispatching through its vtable. Owns everything that is
 * policy rather than hardware: capability gating, the user volume curve
 * (linear-in-dB, delivered as a fading software gain through an audio_dsp
 * instance), mute (a software fade — hardware mute is reserved for power
 * transitions), the DSP voicing mode (Auto: board profile re-applied on route
 * changes; Manual: flat init, app-driven; Disable: no DSP), the speaker route
 * policy (ON/AUTO/OFF + the headphone poll task + insert callback), the
 * click-free idempotent open/close sequencing (see inc_private/bsp_audio.h),
 * and the tone synth fallback (CAP_PCM without CAP_TONE).
 */

#include "bsp.h"
#include "bsp_audio.h"
#include <math.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846f
#endif

static const char *TAG = "BSP_AUDIO";

#define BSP_VOLUME_FADE_MS  100
#define BSP_OPEN_FADE_MS    50
#define BSP_VOLUME_DB_SPAN  40.0f   /* vol=1 → -40 dB, vol=100 → 0 dB */
#define BSP_VOLUME_MAX      150     /* max accepted volume; 100..MAX amplifies */
#define BSP_VOLUME_BOOST_DB 6.0f    /* vol=BSP_VOLUME_MAX → +6 dB (≈2x amplitude) */
#define BSP_AMP_SETTLE_MS   50      /* DAC analog settle before touching the amp */
#define BSP_MUTE_SETTLE_MS  20      /* hw-mute settle before stopping clocks */
#define BSP_NOMINAL_RATE    48000   /* DSP placeholder rate until the first open */

/* Tone synth fallback (CAP_PCM without CAP_TONE): a fixed low-rate mono format
 * chosen for simplicity, not fidelity — it's a beep, not program audio. */
#define TONE_SAMPLE_RATE    16000
#define TONE_BITS           16
#define TONE_CHANNELS       1
#define TONE_CHUNK_MS       10
#define TONE_ENVELOPE_MS    5
#define TONE_DEFAULT_VOLUME 80      /* used only when the app never set a volume */
#define TONE_CHUNK_SAMPLES  (TONE_SAMPLE_RATE * TONE_CHUNK_MS / 1000)
#define TONE_NO_DEADLINE    UINT32_MAX

static bsp_audio_t *s_audio;
static audio_dsp_t  s_dsp;
static bsp_audio_dsp_mode_t s_dsp_mode = BSP_AUDIO_DSP_MODE_AUTO;
static bool s_dsp_bypass;        /* stream format the DSP can't process (bits != 16) */
static bool s_open;              /* app stream running (between open and close) */
static bool s_armed;             /* first open done → amp may follow route policy */
static uint32_t s_rate = BSP_NOMINAL_RATE;   /* current stream rate (profile design fs) */
/* Current app-stream format, for the idempotent-open same-format check. */
static uint32_t s_cur_rate;
static uint8_t  s_cur_bits;
static uint8_t  s_cur_channels;
/* User-facing volume tracked here; hardware codec volume is pinned to max at
 * open and the DSP applies the user value as a software gain (with fade). */
static int  s_volume = -1;       /* -1 = never set → silent */
static bool s_mute;
/* App override for the EQ enable. -1 = follow the board profile's eq_enabled
 * (DSP_MODE_AUTO default); 0/1 = the app forced it, which survives route
 * re-voicing (see apply_dsp_profile). */
static int  s_eq_override = -1;

static volatile bsp_audio_speaker_mode_t s_speaker_mode = BSP_AUDIO_SPEAKER_MODE_ON;
static TaskHandle_t s_route_task;
static portMUX_TYPE s_hp_mux = portMUX_INITIALIZER_UNLOCKED;
static bsp_audio_headphone_cb_t s_hp_cb;
static void *s_hp_cb_arg;
static bool s_hp_last;
static bool s_hp_last_valid;

static void tone_synth_request_stop(bool wait);

static bool hp_inserted_now(void) {
    if (!s_audio || !s_audio->headphone_inserted) return false;
    bool hp = false;
    if (s_audio->headphone_inserted(s_audio, &hp) != ESP_OK) return false;
    return hp;
}

static void apply_speaker_with_hp(bsp_audio_speaker_mode_t mode, bool hp) {
    if (!s_audio || !s_audio->set_speaker_enabled) return;
    bool desired;
    switch (mode) {
        case BSP_AUDIO_SPEAKER_MODE_ON:   desired = true; break;
        case BSP_AUDIO_SPEAKER_MODE_AUTO: desired = !hp;  break;
        case BSP_AUDIO_SPEAKER_MODE_OFF:
        default:                          desired = false; break;
    }
    /* Until the first open the DAC has never produced settled silence, so the
     * amp stays off regardless of policy (the click-free contract). */
    s_audio->set_speaker_enabled(s_audio, desired && s_armed);
}

static void apply_speaker(bsp_audio_speaker_mode_t mode) {
    apply_speaker_with_hp(mode, hp_inserted_now());
}

/* DSP_MODE_AUTO: pull the board's tuning for the current route and apply it.
 * Callable from the app threads (open/reconfig) and the route task — the DSP
 * setters serialize internally. */
static void apply_dsp_profile(bool hp) {
    if (!s_dsp || s_dsp_mode != BSP_AUDIO_DSP_MODE_AUTO) return;
    if (!s_audio || !s_audio->get_dsp_profile) return;
    bsp_audio_dsp_profile_t profile = {0};
    if (s_audio->get_dsp_profile(s_audio, hp, s_rate, &profile) != ESP_OK) return;
    if (profile.num_stages > BSP_AUDIO_DSP_PROFILE_MAX_STAGES) return;
    audio_dsp_set_biquads(s_dsp, profile.biquads, profile.num_stages);
    /* App override wins over the board profile so an HP insert/remove re-voicing
     * doesn't clobber the user's EQ on/off choice. */
    audio_dsp_set_eq_enabled(s_dsp, s_eq_override >= 0 ? (bool)s_eq_override
                                                       : profile.eq_enabled);
    audio_dsp_set_mono_mix(s_dsp, profile.mono_mix);
}

static void route_task(void *arg) {
    (void)arg;
    while (1) {
        bsp_audio_speaker_mode_t mode = s_speaker_mode;
        bool hp = hp_inserted_now();

        /* Detect HP state change: re-voice the DSP for the new route (Auto)
         * and dispatch the user callback (fired outside the critical section
         * so user code can take its time / call into BSP). */
        bsp_audio_headphone_cb_t cb = NULL;
        void *cb_arg = NULL;
        bool changed = false;
        portENTER_CRITICAL(&s_hp_mux);
        if (s_hp_last_valid && hp != s_hp_last) {
            changed = true;
            cb = s_hp_cb;
            cb_arg = s_hp_cb_arg;
        }
        s_hp_last = hp;
        s_hp_last_valid = true;
        portEXIT_CRITICAL(&s_hp_mux);
        if (changed) {
            apply_dsp_profile(hp);
            if (cb) cb(hp, cb_arg);
        }

        apply_speaker_with_hp(mode, hp);

        bool dsp_auto = s_dsp && s_dsp_mode == BSP_AUDIO_DSP_MODE_AUTO &&
                        s_audio && s_audio->get_dsp_profile;
        bool need_poll = (mode == BSP_AUDIO_SPEAKER_MODE_AUTO) || (s_hp_cb != NULL) || dsp_auto;
        TickType_t wait = need_poll ? pdMS_TO_TICKS(200) : portMAX_DELAY;
        ulTaskNotifyTake(pdTRUE, wait);
    }
}

static esp_err_t start_route_task_once(void) {
    if (s_route_task) return ESP_OK;
    return xTaskCreate(route_task, "bsp_audio_rt", 2048, NULL, 1, &s_route_task) == pdPASS
        ? ESP_OK : ESP_ERR_NO_MEM;
}

/* The route task is needed whenever something tracks the headphone state:
 * the AUTO speaker policy, a user insert callback, or Auto DSP voicing. */
static bool route_task_needed(void) {
    if (!s_audio || !(s_audio->caps & BSP_AUDIO_CAP_HEADPHONE)) return false;
    return s_speaker_mode == BSP_AUDIO_SPEAKER_MODE_AUTO || s_hp_cb ||
           (s_dsp && s_dsp_mode == BSP_AUDIO_DSP_MODE_AUTO && s_audio->get_dsp_profile);
}

static float volume_to_gain(int volume) {
    /* Linear-in-dB curve. vol<=0 is a hard zero so muting via volume=0 is true
     * silence. 1..100 attenuates (vol=1 → -40 dB, vol=100 → 0 dB / gain 1.0);
     * 100..MAX is a gentler boost above unity (vol=MAX → +BOOST dB, a digital
     * gain > 1 — useful when the source is quiet, capped to avoid clipping). */
    if (volume <= 0) return 0.0f;
    if (volume > BSP_VOLUME_MAX) volume = BSP_VOLUME_MAX;
    float db;
    if (volume <= 100)
        db = (volume - 100) * (BSP_VOLUME_DB_SPAN / 100.0f);
    else
        db = (volume - 100) * (BSP_VOLUME_BOOST_DB / (BSP_VOLUME_MAX - 100));
    return powf(10.0f, db / 20.0f);
}

static float current_target_gain(void) {
    return s_mute ? 0.0f : volume_to_gain(s_volume);
}

void bsp_audio_set_active(bsp_audio_t *audio, const bsp_audio_init_t *init) {
    tone_synth_request_stop(true);
    if (s_dsp) {
        audio_dsp_deinit(s_dsp);
        s_dsp = NULL;
    }
    s_audio = audio;
    s_dsp_mode = BSP_AUDIO_DSP_MODE_AUTO;
    s_dsp_bypass = false;
    s_open = false;
    s_armed = false;
    s_rate = BSP_NOMINAL_RATE;
    s_cur_rate = 0;
    s_cur_bits = 0;
    s_cur_channels = 0;
    s_volume = -1;
    s_mute = false;
    s_eq_override = -1;  /* follow the board profile until the app forces it */
    if (!audio) return;

    static const bsp_audio_init_t defaults = {0};
    if (!init) init = &defaults;
    s_dsp_mode = init->dsp_mode;
    s_speaker_mode = init->speaker_mode;

    if ((audio->caps & BSP_AUDIO_CAP_PCM) && s_dsp_mode != BSP_AUDIO_DSP_MODE_DISABLE) {
        /* Created flat at a nominal rate so bsp_audio_dsp() is valid from
         * boot; open() reconfigures to the real stream format. */
        audio_dsp_config_t dsp_cfg = {
            .sample_rate     = BSP_NOMINAL_RATE,
            .channels        = 2,
            .bits_per_sample = 16,
        };
        esp_err_t err = audio_dsp_init(&dsp_cfg, &s_dsp);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "audio_dsp_init failed: %d (DSP disabled)", err);
            s_dsp = NULL;
        } else {
            audio_dsp_set_gain(s_dsp, 0.0f, 0);
            apply_dsp_profile(hp_inserted_now());  /* Auto: board voicing from boot */
        }
    }

    if (route_task_needed()) {
        if (start_route_task_once() != ESP_OK) {
            ESP_LOGW(TAG, "route task start failed (HP tracking degraded)");
        }
    }
}

void bsp_audio_quiesce(void) {
    if (!s_audio) return;
    tone_synth_request_stop(true);
    if ((s_audio->caps & BSP_AUDIO_CAP_TONE) && s_audio->tone_stop) s_audio->tone_stop(s_audio);
    s_armed = false;
    if (s_audio->set_hw_mute) s_audio->set_hw_mute(s_audio, true);
    /* Drop the amp gate directly rather than through the route task — it may
     * not get scheduled before the caller cuts power, and the hardware write
     * is what actually needs to land. */
    if (s_audio->set_speaker_enabled) s_audio->set_speaker_enabled(s_audio, false);
}

uint32_t bsp_audio_get_caps(void) {
    return s_audio ? s_audio->caps : 0;
}

static bool pcm_available(void) {
    return s_audio && (s_audio->caps & BSP_AUDIO_CAP_PCM);
}

/* Bring a freshly (re)started stream up click-free: the DSP gain fades in
 * from silence so the clock/format transition never carries a step, the
 * codec is unmuted while the SW gain holds that silence, and — first open
 * only — the amp gate is finally armed once the DAC has settled. */
static void stream_started(uint32_t rate, uint8_t bits, uint8_t ch) {
    s_rate = rate ? rate : BSP_NOMINAL_RATE;
    if (s_dsp) {
        s_dsp_bypass = (bits != 16);
        if (!s_dsp_bypass) {
            audio_dsp_reconfig(s_dsp, s_rate, ch ? ch : 2, 16);
            apply_dsp_profile(hp_inserted_now());
            audio_dsp_set_gain(s_dsp, 0.0f, 0);
            /* Pin the codec to max while the SW gain holds silence; user
             * volume is delivered by the fade below. */
            if (s_audio->set_hw_volume) s_audio->set_hw_volume(s_audio, 100);
            audio_dsp_set_gain(s_dsp, current_target_gain(), BSP_OPEN_FADE_MS);
        }
    }
    if ((!s_dsp || s_dsp_bypass) && s_audio->set_hw_volume) {
        /* No DSP on this stream → user volume lives on the codec (0..100, no
         * amplification — the >100 boost only exists on the SW gain path). */
        int hw = s_volume < 0 ? 0 : (s_volume > 100 ? 100 : s_volume);
        s_audio->set_hw_volume(s_audio, hw);
        if (s_mute && s_audio->set_hw_mute) s_audio->set_hw_mute(s_audio, true);
    }
    if (!s_mute && s_audio->set_hw_mute) s_audio->set_hw_mute(s_audio, false);

    if (!s_armed) {
        s_armed = true;
        if (s_audio->set_speaker_enabled) {
            /* Give the analog stage a moment to settle on first power-up,
             * then let the amp follow the route policy. The amp's own startup
             * transient is unavoidable but is the only remaining click. */
            vTaskDelay(pdMS_TO_TICKS(BSP_AMP_SETTLE_MS));
            apply_speaker(s_speaker_mode);
        }
    }
}

/* Quiet the DAC before its clocks change: hw mute is the one silencing step
 * that doesn't depend on the app writing more buffers (a SW fade would). */
static void stream_stopping(void) {
    if (s_audio->set_hw_mute) {
        s_audio->set_hw_mute(s_audio, true);
        vTaskDelay(pdMS_TO_TICKS(BSP_MUTE_SETTLE_MS));
    }
}

/* Tone synth fallback (CAP_PCM without CAP_TONE): a lazily-created task owns
 * the provider stream while a tone plays — open + stream_started at a fixed
 * format (so amp arming / hw mute follow the app-stream rules), a gain-scaled
 * sine straight into the provider's write() (bypassing the app's audio_dsp
 * chain — the tone owns its own gain), then close. Parked on a notification
 * between tones; a retrigger just updates freq/deadline, re-read every chunk. */

static TaskHandle_t s_tone_task;
static portMUX_TYPE s_tone_mux = portMUX_INITIALIZER_UNLOCKED;
static volatile uint32_t s_tone_freq;
static volatile uint32_t s_tone_deadline_ms;  /* absolute now_ms() deadline; TONE_NO_DEADLINE = until stopped */
static volatile bool s_tone_active;           /* a play session is requested or running */
static volatile bool s_tone_stop_requested;
static volatile bool s_tone_stream_open;      /* provider stream currently owned by the tone task */

static uint32_t now_ms(void) {
    return (uint32_t)(xTaskGetTickCount() * portTICK_PERIOD_MS);
}

/* The app's volume starts unset (silent); fall back to a default so the tone
 * is audible out of the box, without mutating the stored s_volume. */
static float tone_gain(void) {
    if (s_mute) return 0.0f;
    return volume_to_gain(s_volume < 0 ? TONE_DEFAULT_VOLUME : s_volume);
}

static void tone_task(void *arg) {
    (void)arg;
    for (;;) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);

        /* Claim the stream inside the same critical section that reads the
         * request, so tone_synth_request_stop(wait) covers the whole session
         * including the open itself. */
        portENTER_CRITICAL(&s_tone_mux);
        bool active = s_tone_active && !s_tone_stop_requested;
        uint32_t freq = s_tone_freq;
        uint32_t deadline = s_tone_deadline_ms;
        if (active) s_tone_stream_open = true;
        portEXIT_CRITICAL(&s_tone_mux);
        if (!active) {
            portENTER_CRITICAL(&s_tone_mux);
            s_tone_active = false;
            s_tone_stop_requested = false;
            portEXIT_CRITICAL(&s_tone_mux);
            continue;
        }

        if (s_audio->open(s_audio, TONE_SAMPLE_RATE, TONE_BITS, TONE_CHANNELS) != ESP_OK) {
            s_tone_stream_open = false;
            portENTER_CRITICAL(&s_tone_mux);
            s_tone_active = false;
            s_tone_stop_requested = false;
            portEXIT_CRITICAL(&s_tone_mux);
            continue;
        }
        stream_started(TONE_SAMPLE_RATE, TONE_BITS, TONE_CHANNELS);

        const uint32_t attack_samples = TONE_SAMPLE_RATE * TONE_ENVELOPE_MS / 1000;
        const uint32_t release_samples = attack_samples;
        float phase = 0.0f;
        uint32_t elapsed = 0;
        bool stopping = false;
        uint32_t release_left = 0;
        bool done = false;

        while (!done) {
            bool stop_req;
            portENTER_CRITICAL(&s_tone_mux);
            freq = s_tone_freq;
            deadline = s_tone_deadline_ms;
            stop_req = s_tone_stop_requested;
            portEXIT_CRITICAL(&s_tone_mux);
            if (!stopping && (stop_req || (deadline != TONE_NO_DEADLINE && now_ms() >= deadline))) {
                stopping = true;
                release_left = release_samples;
            }

            const float gain = tone_gain();
            const float step = 2.0f * (float)M_PI * (float)freq / (float)TONE_SAMPLE_RATE;
            int16_t buf[TONE_CHUNK_SAMPLES];
            size_t n = 0;
            for (; n < TONE_CHUNK_SAMPLES; n++) {
                float env = elapsed < attack_samples ? (float)elapsed / (float)attack_samples : 1.0f;
                if (stopping) {
                    if (release_left == 0) { done = true; break; }
                    float rel = (float)release_left / (float)release_samples;
                    if (rel < env) env = rel;
                    release_left--;
                }
                float s = sinf(phase) * gain * env * 32767.0f;
                phase += step;
                if (phase > 2.0f * (float)M_PI) phase -= 2.0f * (float)M_PI;
                if (s >  32767.0f) s =  32767.0f;
                if (s < -32768.0f) s = -32768.0f;
                buf[n] = (int16_t)s;
                elapsed++;
            }
            if (n > 0) s_audio->write(s_audio, buf, n * sizeof(int16_t));
        }

        stream_stopping();
        s_audio->close(s_audio);
        s_tone_stream_open = false;
        portENTER_CRITICAL(&s_tone_mux);
        s_tone_active = false;
        s_tone_stop_requested = false;
        portEXIT_CRITICAL(&s_tone_mux);
    }
}

static esp_err_t start_tone_task_once(void) {
    if (s_tone_task) return ESP_OK;
    return xTaskCreate(tone_task, "bsp_audio_tone", 3072, NULL, 1, &s_tone_task) == pdPASS
        ? ESP_OK : ESP_ERR_NO_MEM;
}

static esp_err_t tone_synth_play(uint32_t freq_hz, uint32_t duration_ms) {
    esp_err_t err = start_tone_task_once();
    if (err != ESP_OK) return err;
    portENTER_CRITICAL(&s_tone_mux);
    s_tone_freq = freq_hz;
    s_tone_deadline_ms = duration_ms ? now_ms() + duration_ms : TONE_NO_DEADLINE;
    s_tone_stop_requested = false;
    s_tone_active = true;
    portEXIT_CRITICAL(&s_tone_mux);
    xTaskNotifyGive(s_tone_task);
    return ESP_OK;
}

/* wait=true blocks until the provider stream is no longer owned by the tone
 * task — used before the app reclaims it (open) or on a power transition
 * (quiesce), where a lingering write would race the caller. */
static void tone_synth_request_stop(bool wait) {
    if (!s_tone_task) return;
    portENTER_CRITICAL(&s_tone_mux);
    bool active = s_tone_active;
    s_tone_stop_requested = true;
    portEXIT_CRITICAL(&s_tone_mux);
    if (!active) return;
    xTaskNotifyGive(s_tone_task);
    if (wait) {
        while (s_tone_stream_open) vTaskDelay(pdMS_TO_TICKS(1));
    }
}

esp_err_t bsp_audio_open(uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels) {
    if (!pcm_available() || !s_audio->open) return ESP_ERR_NOT_SUPPORTED;
    tone_synth_request_stop(true);  /* no mixing: reclaim the stream from a playing tone */

    bool same_format = sample_rate == s_cur_rate && bits_per_sample == s_cur_bits &&
                        channels == s_cur_channels;
    if (s_open && same_format) return ESP_OK;
    if (s_open) stream_stopping();

    esp_err_t err = s_audio->open(s_audio, sample_rate, bits_per_sample, channels);
    if (err != ESP_OK) return err;
    s_open = true;
    s_cur_rate = sample_rate;
    s_cur_bits = bits_per_sample;
    s_cur_channels = channels;
    stream_started(sample_rate, bits_per_sample, channels);
    return ESP_OK;
}

esp_err_t bsp_audio_close(void) {
    if (!pcm_available() || !s_audio->close) return ESP_ERR_NOT_SUPPORTED;
    if (!s_open) return ESP_OK;
    stream_stopping();
    esp_err_t err = s_audio->close(s_audio);
    if (err == ESP_OK) s_open = false;
    return err;
}

esp_err_t bsp_audio_write(void *data, size_t len) {
    if (!pcm_available() || !s_audio->write) return ESP_ERR_NOT_SUPPORTED;
    if (!s_open) return ESP_ERR_INVALID_STATE;
    if (s_dsp && !s_dsp_bypass) audio_dsp_process(s_dsp, data, len);
    return s_audio->write(s_audio, data, len);
}

esp_err_t bsp_audio_set_volume(int volume) {
    if (!pcm_available()) return ESP_ERR_NOT_SUPPORTED;
    if (volume < 0)               volume = 0;
    if (volume > BSP_VOLUME_MAX)  volume = BSP_VOLUME_MAX;
    if (volume == s_volume) return ESP_OK;  /* drop slider duplicates */
    s_volume = volume;
    if (s_dsp && !s_dsp_bypass) {
        if (s_mute || !s_open) return ESP_OK;  /* applied on unmute / open fade-in */
        return audio_dsp_set_gain(s_dsp, volume_to_gain(volume), BSP_VOLUME_FADE_MS);
    }
    if (!s_open) return ESP_OK;  /* applied by stream_started */
    /* No DSP on this stream → fall back to direct hardware volume (clicky). The
     * codec volume is 0..100 and cannot amplify, so the >100 boost is dropped. */
    int hw = volume > 100 ? 100 : volume;
    return s_audio->set_hw_volume ? s_audio->set_hw_volume(s_audio, hw)
                                  : ESP_ERR_NOT_SUPPORTED;
}

int bsp_audio_get_volume(void) {
    return s_volume < 0 ? 0 : s_volume;
}

esp_err_t bsp_audio_set_mute(bool mute) {
    if (!pcm_available()) return ESP_ERR_NOT_SUPPORTED;
    if (mute == s_mute) return ESP_OK;
    s_mute = mute;
    if (!s_open) return ESP_OK;  /* applied by stream_started */
    if (s_dsp && !s_dsp_bypass) {
        return audio_dsp_set_gain(s_dsp, current_target_gain(), BSP_VOLUME_FADE_MS);
    }
    return s_audio->set_hw_mute ? s_audio->set_hw_mute(s_audio, mute)
                                : ESP_ERR_NOT_SUPPORTED;
}

bool bsp_audio_get_mute(void) {
    return s_mute;
}

audio_dsp_t bsp_audio_dsp(void) {
    return s_dsp;
}

esp_err_t bsp_audio_set_eq_enabled(bool enabled) {
    if (!s_dsp) return ESP_ERR_NOT_SUPPORTED;  /* no DSP (DISABLE mode / no PCM) */
    /* Record the override so route re-voicing (apply_dsp_profile) keeps it, then
     * apply it now. In MANUAL mode there is no re-voicing, but the override still
     * gives a single, consistent entry point alongside bsp_audio_dsp(). */
    s_eq_override = enabled ? 1 : 0;
    return audio_dsp_set_eq_enabled(s_dsp, enabled);
}

bool bsp_audio_get_eq_enabled(void) {
    return s_dsp ? audio_dsp_is_eq_enabled(s_dsp) : false;
}

esp_err_t bsp_audio_set_speaker_mode(bsp_audio_speaker_mode_t mode) {
    if (mode != BSP_AUDIO_SPEAKER_MODE_ON && mode != BSP_AUDIO_SPEAKER_MODE_AUTO &&
        mode != BSP_AUDIO_SPEAKER_MODE_OFF) return ESP_ERR_INVALID_ARG;
    if (!s_audio || !(s_audio->caps & BSP_AUDIO_CAP_SPEAKER)) return ESP_ERR_NOT_SUPPORTED;
    if (mode == BSP_AUDIO_SPEAKER_MODE_AUTO &&
        !(s_audio->caps & BSP_AUDIO_CAP_HEADPHONE)) return ESP_ERR_NOT_SUPPORTED;
    s_speaker_mode = mode;
    if (mode == BSP_AUDIO_SPEAKER_MODE_AUTO) {
        esp_err_t err = start_route_task_once();
        if (err != ESP_OK) return err;
    }
    if (s_route_task) {
        xTaskNotifyGive(s_route_task);  /* task re-evaluates + re-arms wait */
    } else {
        apply_speaker(mode);
    }
    return ESP_OK;
}

bsp_audio_speaker_mode_t bsp_audio_get_speaker_mode(void) {
    return s_speaker_mode;
}

bool bsp_audio_headphone_inserted(void) {
    return hp_inserted_now();
}

esp_err_t bsp_audio_set_headphone_callback(bsp_audio_headphone_cb_t cb, void *user) {
    if (!s_audio || !(s_audio->caps & BSP_AUDIO_CAP_HEADPHONE)) return ESP_ERR_NOT_SUPPORTED;
    portENTER_CRITICAL(&s_hp_mux);
    s_hp_cb_arg = user;
    s_hp_cb     = cb;
    portEXIT_CRITICAL(&s_hp_mux);
    if (cb) {
        esp_err_t err = start_route_task_once();
        if (err != ESP_OK) return err;
        xTaskNotifyGive(s_route_task);  /* re-evaluate need_poll */
    }
    return ESP_OK;
}

esp_err_t bsp_audio_tone(uint32_t freq_hz, uint32_t duration_ms) {
    if (!s_audio) return ESP_ERR_NOT_SUPPORTED;
    if (s_audio->caps & BSP_AUDIO_CAP_TONE) {
        return s_audio->tone ? s_audio->tone(s_audio, freq_hz, duration_ms) : ESP_ERR_NOT_SUPPORTED;
    }
    if (pcm_available()) {
        if (s_open) return ESP_ERR_INVALID_STATE;  /* no mixing with an app stream */
        return tone_synth_play(freq_hz, duration_ms);
    }
    return ESP_ERR_NOT_SUPPORTED;
}

esp_err_t bsp_audio_tone_stop(void) {
    if (!s_audio) return ESP_ERR_NOT_SUPPORTED;
    if (s_audio->caps & BSP_AUDIO_CAP_TONE) {
        return s_audio->tone_stop ? s_audio->tone_stop(s_audio) : ESP_ERR_NOT_SUPPORTED;
    }
    if (pcm_available()) {
        tone_synth_request_stop(false);
        return ESP_OK;
    }
    return ESP_ERR_NOT_SUPPORTED;
}
