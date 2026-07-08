/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 simulator board: the host-side counterpart of paper_s3.c.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "bsp_dispatch.h"
#include "bsp_rtc.h"
#include "bsp_audio.h"
#include "bsp_power.h"
#include "sdl_panel.h"
#include "rtc_sim.h"
#include "power_sim.h"
#include "sdl_audio.h"
#include <stdio.h>
#include <stdlib.h>

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);
    sdl_panel_config_t sdl_config = {
        .title     = "M5PaperS3",
        .type      = BSP_DISPLAY_TYPE_DIRECT_EPD,
        .size      = { 960, 540 },
        .format    = BSP_PIXEL_FORMAT_L8,
        .scale_div = 2,
    };

    bsp_display_t *display = NULL;
    bsp_touch_t   *touch   = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, &touch);
    if (err != ESP_OK) return err;

    bsp_display_set_active(display);
    bsp_touch_set_active(touch);

    bsp_rtc_t *rtc = NULL;
    if (rtc_sim_create(&rtc) == ESP_OK) bsp_rtc_set_active(rtc);

    bsp_power_t *power = NULL;
    if (power_sim_create(&power) == ESP_OK) bsp_power_set_active(power);

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

esp_err_t bsp_power_off(void) { exit(0); }
