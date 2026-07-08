/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Air Quality Kit simulator board: host-side counterpart of airq.c. The
 * SDL backend mimics the 200x200 B/W EPD so the shared app renders identically,
 * and the tone-only SDL audio stands in for the GPIO9 passive buzzer. The front
 * buttons are physical-only, so they stay unwired here; power controls fall back
 * to the shared defaults.
 * Build with -DBSP_BOARD=airq.
 */

#include "bsp.h"
#include "bsp_audio.h"
#include "bsp_display.h"
#include "bsp_dispatch.h"
#include "sdl_audio.h"
#include "sdl_panel.h"
#include <stdio.h>

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);
    sdl_panel_config_t sdl_config = {
        .title     = "M5 AirQ",
        .type      = BSP_DISPLAY_TYPE_SPI_EPD,
        .size      = { 200, 200 },
        .format    = BSP_PIXEL_FORMAT_L8,
        .scale_div = 1,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, NULL);
    if (err != ESP_OK) return err;

    bsp_display_set_active(display);

    bsp_audio_t *audio = NULL;
    esp_err_t audio_err = sdl_audio_create(&(sdl_audio_config_t){ .tone_only = true }, &audio);
    if (audio_err == ESP_OK) {
        bsp_audio_set_active(audio, &(bsp_audio_init_t){
            .dsp_mode = config->audio.dsp_mode,
            .speaker_mode = config->audio.speaker_mode,
        });
    } else {
        fprintf(stderr, "[sim] sdl_audio_create failed: %d\n", audio_err);
    }
    return ESP_OK;
}
