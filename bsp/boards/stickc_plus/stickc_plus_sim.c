/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5StickC-Plus simulator board: host-side counterpart of stickc_plus.c. The
 * SDL backend mimics the 135x240 RGB565 SPI panel; a fake battery backs the
 * power seam, and the tone-only SDL audio stands in for the GPIO2 passive
 * buzzer. Build with -DBSP_BOARD=stickc_plus.
 */

#include "bsp.h"
#include "bsp_audio.h"
#include "bsp_display.h"
#include "bsp_dispatch.h"
#include "bsp_power.h"
#include "sdl_audio.h"
#include "sdl_panel.h"
#include "power_sim.h"
#include <stdio.h>

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    const sdl_panel_config_t sdl_config = {
        .title     = "M5StickC-Plus",
        .type      = BSP_DISPLAY_TYPE_SPI,
        .size      = { 135, 240 },
        .format    = BSP_PIXEL_FORMAT_RGB565,
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

    bsp_power_t *power = NULL;
    if (power_sim_create(&power) == ESP_OK) bsp_power_set_active(power);
    return ESP_OK;
}

esp_err_t bsp_power_off(void) {
    return ESP_FAIL;
}
