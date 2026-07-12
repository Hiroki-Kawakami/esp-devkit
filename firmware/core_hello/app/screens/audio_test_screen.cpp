/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Audio output test: BtnA plays a short PCM sine arpeggio through bsp_audio
 * (open/write/close on a worker task so the blocking write() never stalls the
 * UI), BtnB cycles the volume and a mute step so the software-gain path is
 * exercised too. Works on Core2 (NS4168 over I2S) and Basic (built-in DAC), and
 * on the host simulator (SDL).
 */

#include "audio_test_screen.hpp"
#include <math.h>
#include <cstdio>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846f
#endif

namespace {

AudioTestScreen *g_active = nullptr;

constexpr int      kVolumes[]   = { 25, 50, 75, 100 };
constexpr int      kMuteIndex   = 4;   /* one past the volume steps */
constexpr uint32_t kSampleRate  = 44100;
constexpr int      kChunkFrames = 512;
constexpr uint32_t kNoteMs      = 250;
constexpr uint32_t kFadeMs      = 6;

volatile bool g_playing = false;
volatile bool g_stop    = false;

float note_envelope(uint32_t i, uint32_t total) {
    const uint32_t fade = kSampleRate * kFadeMs / 1000;
    if (i < fade)           return (float)i / (float)fade;
    if (i > total - fade)   return (float)(total - i) / (float)fade;
    return 1.0f;
}

void play_note(float freq) {
    const uint32_t total = kSampleRate * kNoteMs / 1000;
    int16_t buf[kChunkFrames * 2];
    float phase = 0.0f;
    const float step = 2.0f * (float)M_PI * freq / (float)kSampleRate;
    for (uint32_t i = 0; i < total && !g_stop; ) {
        uint32_t n = total - i;
        if (n > kChunkFrames) n = kChunkFrames;
        for (uint32_t k = 0; k < n; k++) {
            float s = sinf(phase) * 0.30f * note_envelope(i + k, total);
            int16_t v = (int16_t)(s * 32767.0f);
            buf[k * 2]     = v;
            buf[k * 2 + 1] = v;
            phase += step;
            if (phase > 2.0f * (float)M_PI) phase -= 2.0f * (float)M_PI;
        }
        bsp_audio_write(buf, n * 2 * sizeof(int16_t));
        i += n;
    }
}

void play_task(void *) {
    if (bsp_audio_open(kSampleRate, 16, 2) == ESP_OK) {
        static const float notes[] = { 261.63f, 329.63f, 392.00f, 523.25f };
        for (float f : notes) {
            if (g_stop) break;
            play_note(f);
        }
        bsp_audio_close();
    }
    g_playing = false;
    lv_async_call([](void *) { if (g_active) g_active->set_status("idle"); }, nullptr);
    vTaskDelete(nullptr);
}

}

void AudioTestScreen::build() {
    lv_obj_set_style_bg_color(root_, lv_color_black(), 0);
    lv_obj_set_style_pad_all(root_, 8, 0);
    lv_obj_set_style_pad_row(root_, 8, 0);
    lv_obj_remove_flag(root_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);

    lv_obj_t *title = lv_label_create(root_);
    lv_label_set_text(title, "Audio Test");
    lv_obj_set_style_text_font(title, &lv_font_montserrat_24, 0);
    lv_obj_set_style_text_color(title, lv_color_hex(0x00E0A0), 0);

    caps_label_ = lv_label_create(root_);
    lv_obj_set_style_text_color(caps_label_, lv_color_hex(0xB0B0B0), 0);

    volume_label_ = lv_label_create(root_);
    lv_obj_set_style_text_color(volume_label_, lv_color_white(), 0);

    status_label_ = lv_label_create(root_);
    lv_obj_set_style_text_color(status_label_, lv_color_white(), 0);
    lv_label_set_text(status_label_, "idle");

    lv_obj_t *hint = lv_label_create(root_);
    lv_obj_set_style_text_color(hint, lv_color_hex(0x808080), 0);
    lv_label_set_text(hint, "BtnA: play   BtnB: volume");
}

void AudioTestScreen::onAppear() {
    g_active = this;

    bsp_audio_set_speaker_mode(BSP_AUDIO_SPEAKER_MODE_ON);
    bsp_audio_set_mute(false);
    volume_index_ = 2;
    bsp_audio_set_volume(kVolumes[volume_index_]);
    render_caps();
    render_volume();

    uint8_t count = bsp_button_count();
    if (count > 0) {
        bsp_button_on_click(0, [](uint8_t, void *) {
            lv_async_call([](void *) { if (g_active) g_active->start_play(); }, nullptr);
        }, nullptr);
    }
    if (count > 1) {
        bsp_button_on_click(1, [](uint8_t, void *) {
            lv_async_call([](void *) { if (g_active) g_active->cycle_volume(); }, nullptr);
        }, nullptr);
    }
}

void AudioTestScreen::onDisappear() {
    g_stop = true;
    while (g_playing) vTaskDelay(pdMS_TO_TICKS(2));

    uint8_t count = bsp_button_count();
    if (count > 0) bsp_button_on_click(0, nullptr, nullptr);
    if (count > 1) bsp_button_on_click(1, nullptr, nullptr);
    g_active = nullptr;
}

void AudioTestScreen::set_status(const char *text) {
    lv_label_set_text(status_label_, text);
}

void AudioTestScreen::render_caps() {
    uint32_t caps = bsp_audio_get_caps();
    char text[64];
    snprintf(text, sizeof text, "caps:%s%s%s%s",
             caps & BSP_AUDIO_CAP_PCM       ? " PCM"  : "",
             caps & BSP_AUDIO_CAP_TONE      ? " TONE" : "",
             caps & BSP_AUDIO_CAP_SPEAKER   ? " SPK"  : "",
             caps & BSP_AUDIO_CAP_HEADPHONE ? " HP"   : "");
    lv_label_set_text(caps_label_, text);
}

void AudioTestScreen::render_volume() {
    if (volume_index_ == kMuteIndex) {
        lv_label_set_text(volume_label_, "Volume: MUTE");
    } else {
        lv_label_set_text_fmt(volume_label_, "Volume: %d", kVolumes[volume_index_]);
    }
}

void AudioTestScreen::cycle_volume() {
    volume_index_ = (volume_index_ + 1) % (kMuteIndex + 1);
    if (volume_index_ == kMuteIndex) {
        bsp_audio_set_mute(true);
    } else {
        bsp_audio_set_mute(false);
        bsp_audio_set_volume(kVolumes[volume_index_]);
    }
    render_volume();
}

void AudioTestScreen::start_play() {
    if (g_playing) return;
    g_playing = true;
    g_stop = false;
    set_status("playing...");
    if (xTaskCreate(play_task, "audio_test", 8192, nullptr, 4, nullptr) != pdPASS) {
        g_playing = false;
        set_status("task error");
    }
}
