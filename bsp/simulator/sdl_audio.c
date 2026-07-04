/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sdl_audio.h"
#include <SDL2/SDL.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846f
#endif

static const char *TAG = "SDL_AUDIO";

/* Backpressure high-water mark: block write() once this much audio is queued.
 * Mimics the device's I2S DMA depth; keeps producer pacing real-time while
 * bounding latency. */
#define SDL_AUDIO_HIGH_WATER_MS 100

/* Tone-only mode: a fixed mono format is plenty for a buzzer timbre. */
#define TONE_SAMPLE_RATE     44100
#define TONE_CHUNK_MS        10
#define TONE_CHUNK_SAMPLES   (TONE_SAMPLE_RATE * TONE_CHUNK_MS / 1000)
#define TONE_AMPLITUDE       12000
#define TONE_NO_DEADLINE     UINT32_MAX

typedef struct {
    bsp_audio_t base;
    SDL_AudioDeviceID dev;      /* 0 = null sink (headless / no audio device) */
    bool stream_open;
    uint32_t rate;
    uint8_t  channels;
    size_t   frame_bytes;       /* channels * 2 (S16) */
    size_t   high_water;        /* bytes */
    /* "Hardware" silencing flags (codec mute / amp gate): audio keeps being
     * consumed at the same rate, just as zeros — pausing the device instead
     * would stall write()'s backpressure, which a real amp gate never does. */
    bool hw_mute;
    bool speaker_off;
    uint8_t *zero_buf;
    size_t   zero_cap;
    /* Null-sink pacing clock: virtual playback position vs wall clock. */
    uint64_t ns_start_ms;
    uint64_t ns_queued_us;

    /* Tone-only mode: a lazily-created task owns the SDL stream while a tone
     * plays, synthesizing a square wave through sa_open/sa_write/sa_close --
     * the same path a PCM app stream uses. freq/duration are cross-thread
     * state guarded by tone_mux; a retrigger just updates them and wakes the
     * task, which re-reads them every chunk (mirrors bsp_audio.c's own tone
     * synth fallback). */
    bool tone_only;
    TaskHandle_t tone_task;
    portMUX_TYPE tone_mux;
    volatile uint32_t tone_freq;
    volatile uint32_t tone_deadline_ms;  /* absolute tick-ms deadline; NO_DEADLINE = until stopped */
    volatile bool tone_active;
    volatile bool tone_stop_requested;
    volatile bool tone_exit;
    volatile bool tone_task_exited;
} sdl_audio_state_t;

static esp_err_t sa_close(bsp_audio_t *self);

static esp_err_t sa_open(bsp_audio_t *self, uint32_t rate, uint8_t bits, uint8_t ch) {
    sdl_audio_state_t *s = (sdl_audio_state_t *)self;
    if (!rate) rate = 48000;
    if (!bits) bits = 16;
    if (!ch)   ch   = 2;
    if (bits != 16) return ESP_ERR_NOT_SUPPORTED;
    if (ch < 1 || ch > 2) return ESP_ERR_INVALID_ARG;

    if (s->stream_open) sa_close(self);

    s->rate        = rate;
    s->channels    = ch;
    s->frame_bytes = (size_t)ch * 2;
    s->high_water  = (size_t)rate * s->frame_bytes * SDL_AUDIO_HIGH_WATER_MS / 1000;

    if (!getenv("SIMULATOR_HEADLESS")) {
        SDL_AudioSpec want = {0}, have;
        want.freq     = (int)rate;
        want.format   = AUDIO_S16SYS;
        want.channels = ch;
        want.samples  = 1024;
        s->dev = SDL_OpenAudioDevice(NULL, 0, &want, &have, 0);
        if (s->dev) {
            SDL_PauseAudioDevice(s->dev, 0);
        } else {
            ESP_LOGW(TAG, "SDL_OpenAudioDevice failed (%s) — silent null sink", SDL_GetError());
        }
    }
    s->ns_start_ms  = SDL_GetTicks64();
    s->ns_queued_us = 0;
    s->stream_open  = true;
    return ESP_OK;
}

static esp_err_t sa_close(bsp_audio_t *self) {
    sdl_audio_state_t *s = (sdl_audio_state_t *)self;
    if (!s->stream_open) return ESP_ERR_INVALID_STATE;
    if (s->dev) {
        SDL_ClearQueuedAudio(s->dev);
        SDL_CloseAudioDevice(s->dev);
        s->dev = 0;
    }
    s->stream_open = false;
    return ESP_OK;
}

static esp_err_t sa_write(bsp_audio_t *self, const void *data, size_t len) {
    sdl_audio_state_t *s = (sdl_audio_state_t *)self;
    if (!s->stream_open) return ESP_ERR_INVALID_STATE;

    const void *out = data;
    if (s->hw_mute || s->speaker_off) {
        if (s->zero_cap < len) {
            uint8_t *grown = realloc(s->zero_buf, len);
            if (!grown) return ESP_ERR_NO_MEM;
            memset(grown, 0, len);
            s->zero_buf = grown;
            s->zero_cap = len;
        }
        out = s->zero_buf;
    }

    if (s->dev) {
        while (SDL_GetQueuedAudioSize(s->dev) + len > s->high_water) {
            SDL_Delay(2);
        }
        return SDL_QueueAudio(s->dev, out, (Uint32)len) == 0 ? ESP_OK : ESP_FAIL;
    }

    /* Null sink: advance a virtual playback clock and sleep so the producer
     * sees the same real-time backpressure as a sound device. */
    s->ns_queued_us += (uint64_t)len * 1000000u / ((uint64_t)s->rate * s->frame_bytes);
    uint64_t elapsed_us = (SDL_GetTicks64() - s->ns_start_ms) * 1000u;
    uint64_t high_water_us = (uint64_t)SDL_AUDIO_HIGH_WATER_MS * 1000u;
    if (s->ns_queued_us > elapsed_us + high_water_us) {
        SDL_Delay((Uint32)((s->ns_queued_us - elapsed_us - high_water_us) / 1000u));
    }
    return ESP_OK;
}

static esp_err_t sa_set_hw_mute(bsp_audio_t *self, bool mute) {
    ((sdl_audio_state_t *)self)->hw_mute = mute;
    return ESP_OK;
}

static esp_err_t sa_set_speaker_enabled(bsp_audio_t *self, bool enabled) {
    ((sdl_audio_state_t *)self)->speaker_off = !enabled;
    return ESP_OK;
}

/* ---------------- Tone-only square-wave synth ---------------- */

static uint32_t tone_now_ms(void) {
    return (uint32_t)SDL_GetTicks64();
}

static void tone_task(void *arg) {
    sdl_audio_state_t *s = arg;
    for (;;) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);

        portENTER_CRITICAL(&s->tone_mux);
        bool exiting = s->tone_exit;
        bool active = s->tone_active && !s->tone_stop_requested;
        portEXIT_CRITICAL(&s->tone_mux);
        if (exiting) break;
        if (!active) {
            portENTER_CRITICAL(&s->tone_mux);
            s->tone_active = false;
            s->tone_stop_requested = false;
            portEXIT_CRITICAL(&s->tone_mux);
            continue;
        }

        sa_open(&s->base, TONE_SAMPLE_RATE, 16, 1);

        float phase = 0.0f;
        bool done = false;
        while (!done) {
            uint32_t freq, deadline;
            bool stop_req;
            portENTER_CRITICAL(&s->tone_mux);
            freq = s->tone_freq;
            deadline = s->tone_deadline_ms;
            stop_req = s->tone_stop_requested;
            portEXIT_CRITICAL(&s->tone_mux);
            if (stop_req || (deadline != TONE_NO_DEADLINE && tone_now_ms() >= deadline)) {
                done = true;
                break;
            }

            const float step = 2.0f * (float)M_PI * (float)freq / (float)TONE_SAMPLE_RATE;
            int16_t buf[TONE_CHUNK_SAMPLES];
            for (int i = 0; i < TONE_CHUNK_SAMPLES; i++) {
                buf[i] = sinf(phase) >= 0.0f ? TONE_AMPLITUDE : -TONE_AMPLITUDE;
                phase += step;
                if (phase > 2.0f * (float)M_PI) phase -= 2.0f * (float)M_PI;
            }
            sa_write(&s->base, buf, sizeof(buf));
        }

        sa_close(&s->base);
        portENTER_CRITICAL(&s->tone_mux);
        s->tone_active = false;
        s->tone_stop_requested = false;
        portEXIT_CRITICAL(&s->tone_mux);
    }
    s->tone_task_exited = true;
}

static esp_err_t start_tone_task_once(sdl_audio_state_t *s) {
    if (s->tone_task) return ESP_OK;
    return xTaskCreate(tone_task, "sdl_audio_tone", 4096, s, 1, &s->tone_task) == pdPASS
        ? ESP_OK : ESP_ERR_NO_MEM;
}

static esp_err_t sa_tone(bsp_audio_t *self, uint32_t freq_hz, uint32_t duration_ms) {
    sdl_audio_state_t *s = (sdl_audio_state_t *)self;
    if (freq_hz == 0) return ESP_ERR_INVALID_ARG;
    esp_err_t err = start_tone_task_once(s);
    if (err != ESP_OK) return err;
    portENTER_CRITICAL(&s->tone_mux);
    s->tone_freq = freq_hz;
    s->tone_deadline_ms = duration_ms ? tone_now_ms() + duration_ms : TONE_NO_DEADLINE;
    s->tone_stop_requested = false;
    s->tone_active = true;
    portEXIT_CRITICAL(&s->tone_mux);
    xTaskNotifyGive(s->tone_task);
    return ESP_OK;
}

static esp_err_t sa_tone_stop(bsp_audio_t *self) {
    sdl_audio_state_t *s = (sdl_audio_state_t *)self;
    if (!s->tone_task) return ESP_OK;
    portENTER_CRITICAL(&s->tone_mux);
    bool active = s->tone_active;
    s->tone_stop_requested = true;
    portEXIT_CRITICAL(&s->tone_mux);
    if (active) xTaskNotifyGive(s->tone_task);
    return ESP_OK;
}

/* Stop any playing tone and wait for the task to reach its idle wait, then
 * ask it to exit and wait for that too — needed before freeing the state. */
static void tone_task_shutdown(sdl_audio_state_t *s) {
    if (!s->tone_task) return;
    sa_tone_stop(&s->base);
    while (s->tone_active) SDL_Delay(1);
    portENTER_CRITICAL(&s->tone_mux);
    s->tone_exit = true;
    portEXIT_CRITICAL(&s->tone_mux);
    xTaskNotifyGive(s->tone_task);
    while (!s->tone_task_exited) SDL_Delay(1);
}

static esp_err_t sa_deinit(bsp_audio_t *self) {
    sdl_audio_state_t *s = (sdl_audio_state_t *)self;
    if (s->tone_only) tone_task_shutdown(s);
    if (s->stream_open) sa_close(self);
    SDL_QuitSubSystem(SDL_INIT_AUDIO);
    free(s->zero_buf);
    free(s);
    return ESP_OK;
}

esp_err_t sdl_audio_create(const sdl_audio_config_t *config, bsp_audio_t **out_audio) {
    if (!out_audio) return ESP_ERR_INVALID_ARG;

    /* Audio is its own SDL subsystem — independent of the video/timer init the
     * display backend does, and refcounted, so this is safe either way. */
    if (SDL_InitSubSystem(SDL_INIT_AUDIO) != 0) {
        ESP_LOGW(TAG, "SDL_InitSubSystem(AUDIO) failed (%s) — null sink only", SDL_GetError());
    }

    sdl_audio_state_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;
    portMUX_INITIALIZE(&s->tone_mux);

    bool tone_only = config && config->tone_only;
    s->tone_only = tone_only;

    if (tone_only) {
        s->base.caps      = BSP_AUDIO_CAP_TONE;
        s->base.tone      = sa_tone;
        s->base.tone_stop = sa_tone_stop;
    } else {
        s->base.caps                = BSP_AUDIO_CAP_PCM | BSP_AUDIO_CAP_SPEAKER;
        s->base.open                = sa_open;
        s->base.close               = sa_close;
        s->base.write               = sa_write;
        s->base.set_hw_mute         = sa_set_hw_mute;
        s->base.set_speaker_enabled = sa_set_speaker_enabled;
    }
    s->base.deinit = sa_deinit;

    /* Registers closed — the host device opens on the first tone/open. */
    *out_audio = &s->base;
    return ESP_OK;
}
