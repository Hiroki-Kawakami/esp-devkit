/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core simulator board: host-side counterpart of core.c. Build with
 * -DBSP_BOARD=core.
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

    const sdl_panel_config_t sdl_config = {
        .title     = "M5Stack Core",
        .type      = BSP_DISPLAY_TYPE_SPI,
        .size      = { 320, 240 },
        .format    = BSP_PIXEL_FORMAT_RGB565,
        .scale_div = 1,
    };
    bsp_display_t *display = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, NULL);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    bsp_audio_t *audio = NULL;
    esp_err_t audio_err = sdl_audio_create(NULL, &audio);
    if (audio_err == ESP_OK) {
        bsp_audio_set_active(audio, &(bsp_audio_init_t){
            .dsp_mode     = config ? config->audio.dsp_mode : 0,
            .speaker_mode = config ? config->audio.speaker_mode : 0,
        });
    } else {
        fprintf(stderr, "[sim] sdl_audio_create failed: %d\n", audio_err);
    }
    return ESP_OK;
}
