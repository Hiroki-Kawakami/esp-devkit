/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 simulator board: the host-side counterpart of tab5.c. Brings up
 * the shared SDL backend in MIPI mode with the Tab5's native portrait geometry.
 */

#include "bsp.h"
#include "bsp_display.h"
#include "bsp_touch.h"
#include "bsp_dispatch.h"
#include "bsp_audio.h"
#include "bsp_imu.h"
#include "bsp_keyboard.h"
#include "bsp_power.h"
#include "sdl_panel.h"
#include "sim_keyboard.h"
#include "imu_sim.h"
#include "sdl_audio.h"
#include "power_sim.h"
#include <stdio.h>
#include <stdlib.h>

#define TAB5_PANEL_W 720
#define TAB5_PANEL_H 1280

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);
    sdl_panel_config_t sdl_config = {
        .title     = "M5Stack Tab5",
        .type      = BSP_DISPLAY_TYPE_MIPI_DSI,
        .size      = { TAB5_PANEL_W, TAB5_PANEL_H },
        .format    = config->display.pixel_format == BSP_PIXEL_FORMAT_RGB888
                     ? BSP_PIXEL_FORMAT_RGB888 : BSP_PIXEL_FORMAT_RGB565,
        .scale_div = 2,
        .framebuffer = { .fb_num = config->display.fb_num ? config->display.fb_num : 2 },
    };
    bsp_display_t *display = NULL;
    bsp_touch_t   *touch   = NULL;
    esp_err_t err = sdl_panel_create(&sdl_config, &display, &touch);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);
    bsp_touch_set_active(touch);

    bsp_imu_t *imu = NULL;
    if (imu_sim_create(&imu) == ESP_OK) bsp_imu_set_active(imu);

    bsp_audio_t *audio = NULL;
    esp_err_t audio_err = sdl_audio_create(NULL, &audio);  /* PCM | SPEAKER */
    if (audio_err == ESP_OK) {
        bsp_audio_set_active(audio, &(bsp_audio_init_t){
            .dsp_mode     = config->audio.dsp_mode,
            .speaker_mode = config->audio.speaker_mode,
        });
    } else {
        fprintf(stderr, "[sim] sdl_audio_create failed: %d\n", audio_err);
    }

    bsp_power_t *power = NULL;
    if (power_sim_create(&(power_sim_config_t){
            .caps     = BSP_POWER_CAP_BATTERY | BSP_POWER_CAP_EXTERNAL | BSP_POWER_CAP_CHARGE_CTRL
                      | BSP_POWER_CAP_CHARGE_FAST | BSP_POWER_CAP_CHARGE_STAT,
            .empty_mv = 6600,
            .full_mv  = 8200,
        }, &power) == ESP_OK) {
        bsp_power_set_active(power);
    }

    bsp_keyboard_t *keyboard = NULL;
    if (sim_keyboard_create(&keyboard) == ESP_OK) {
        bsp_keyboard_set_active(keyboard, &(bsp_keyboard_init_t){
            .mode   = config->keyboard.mode,
            .detect = config->keyboard.detect,
        });
    }
    return ESP_OK;
}

esp_err_t bsp_power_off(void) { exit(0); }
