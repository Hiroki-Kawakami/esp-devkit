/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Air Quality Kit simulator board: host-side counterpart of airq.c. The
 * SDL backend mimics the 200x200 B/W EPD so the shared app renders identically,
 * and the tone-only SDL audio stands in for the GPIO9 passive buzzer, while the
 * host RTC stands in for the BM8563, and emulated SEN55/SCD40 chips on the
 * idf_compat virtual I2C bus (port 0) stand in for the air sensors —
 * BSP_POWER_SWITCH_SENSOR is a no-op, the rail is always up here. The front
 * buttons are physical-only, so they stay unwired here; the other power
 * controls fall back to the shared defaults. Build with -DBSP_BOARD=airq.
 */

#include "bsp.h"
#include "bsp_audio.h"
#include "bsp_display.h"
#include "bsp_dispatch.h"
#include "bsp_rtc.h"
#include "sdl_audio.h"
#include "sdl_panel.h"
#include "rtc_sim.h"
#include "sen55_sim.h"
#include "scd40_sim.h"
#include "driver/i2c_master.h"
#include <stdio.h>

static i2c_master_bus_handle_t s_i2c_bus;

i2c_master_bus_handle_t bsp_bus_get_i2c_handle(int i2c_port) {
    return i2c_port == 0 ? s_i2c_bus : NULL;
}

esp_err_t bsp_power_set_switch(bsp_power_switch_t sw, bool on) {
    (void)on;
    return sw == BSP_POWER_SWITCH_SENSOR ? ESP_OK : ESP_ERR_NOT_SUPPORTED;
}

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

    bsp_rtc_t *rtc = NULL;
    if (rtc_sim_create(&rtc) == ESP_OK) bsp_rtc_set_active(rtc);

    const i2c_master_bus_config_t i2c_cfg = { .i2c_port = 0, .sda_io_num = -1, .scl_io_num = -1 };
    if (i2c_new_master_bus(&i2c_cfg, &s_i2c_bus) == ESP_OK) {
        sen55_sim_attach(s_i2c_bus, 0, NULL);
        scd40_sim_attach(s_i2c_bus, 0, NULL);
    } else {
        fprintf(stderr, "[sim] i2c_new_master_bus failed\n");
    }
    return ESP_OK;
}
