/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal M5 Air Quality Kit sample: brings up the BSP + LVGL and shows a
 * counter plus live air-quality readings on the GDEY0154D67 EPD. The EPD has no
 * host framebuffer, so LVGL renders into its own L8 buffer and flush blits each
 * region into the panel buffer; the last flush of a render drives the whole
 * panel with s_refresh_mode.
 *
 * Partial-refresh test: the first frame is a full refresh (seeds the panel base
 * image), then the two front buttons drive the counter -- A (top) increments,
 * B decrements -- and the power button (GPIO42) resets it to 0, each as a fast
 * direct-update (BSP_EPD_MODE_FAST) partial refresh.
 *
 * Sensors: the app powers the rail via BSP_POWER_SWITCH_SENSOR, creates the
 * SEN55 + SCD40 (libs/sensors) on the bus from bsp_bus_get_i2c_handle() and
 * polls every second from an LVGL timer; the readings re-render at the SCD40's
 * 5 s cadence to keep EPD refreshes sparse. On the simulator the same code
 * reads the emulated chips.
 */

#include "airq_hello.hpp"
#include "lvgl.hpp"
#include "sen55.h"
#include "scd40.h"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include <assert.h>
#include <cmath>
#include <cstdio>
#include <cstring>

static const char *TAG = "airq_hello";

static lv_display_t  *s_disp;
static uint8_t       *s_buf;
static lv_obj_t      *s_label;
static lv_obj_t      *s_air_label;
static bsp_epd_mode_t s_refresh_mode = BSP_EPD_MODE_QUALITY;

static sen55_t      *s_sen55;
static scd40_t      *s_scd40;
static sen55_data_t  s_air;
static scd40_data_t  s_co2;
static bool          s_air_valid, s_co2_valid;

static void lvgl_init() {
    lvgl_port_cfg_t config = {
        .task_priority    = 4,
        .task_stack       = 8192,
        .task_affinity    = 0,
        .task_max_sleep_ms = 500,
        .task_stack_caps  = MALLOC_CAP_INTERNAL | MALLOC_CAP_DEFAULT,
        .timer_period_ms  = 5,
    };
    esp_err_t err = lvgl_port_init(&config);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "lvgl_port_init: %s", esp_err_to_name(err));
        assert(0);
    }

    const bsp_size_t size = bsp_display_get_size();
    const size_t buf_bytes = (size_t)size.width * size.height / 4;   /* L8, partial */

    s_buf = (uint8_t *)heap_caps_aligned_alloc(4, buf_bytes, MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    assert(s_buf);
    s_disp = lv_display_create(size.width, size.height);
    lv_display_set_color_format(s_disp, LV_COLOR_FORMAT_L8);
    lv_display_set_buffers(s_disp, s_buf, NULL, buf_bytes, LV_DISPLAY_RENDER_MODE_PARTIAL);
    lv_display_set_flush_cb(s_disp, [](lv_display_t *disp, const lv_area_t *area, uint8_t *px_map) {
        lv_area_t rot = *area;
        lv_display_rotate_area(disp, &rot);
        bsp_rect_t rect = { { rot.x1, rot.y1 }, { rot.x2 - rot.x1 + 1, rot.y2 - rot.y1 + 1 } };
        bsp_display_draw_bitmap(rect, px_map, BSP_ROTATION_0);
        if (lv_display_flush_is_last(disp)) {
            bsp_display_refresh({ { 0, 0 }, bsp_display_get_size() }, s_refresh_mode);
        }
        lv_display_flush_ready(disp);
    });
    lv_display_set_default(s_disp);

    /* Draws update the buffer only; the flush drives the panel explicitly. */
    bsp_display_set_epd_mode(BSP_EPD_MODE_NONE);
}

static void build_hello_screen() {
    lv_obj_t *scr = lv_screen_active();
    lv_obj_set_style_bg_color(scr, lv_color_white(), 0);

    s_label = lv_label_create(scr);
    lv_label_set_text(s_label, "Count: 0");
    lv_obj_set_style_text_font(s_label, &lv_font_montserrat_24, 0);
    lv_obj_set_style_text_color(s_label, lv_color_black(), 0);
    lv_obj_align(s_label, LV_ALIGN_TOP_MID, 0, 16);

    s_air_label = lv_label_create(scr);
    lv_label_set_text(s_air_label, "sensors: waiting...");
    lv_obj_set_style_text_color(s_air_label, lv_color_black(), 0);
    lv_obj_align(s_air_label, LV_ALIGN_LEFT_MID, 12, 24);
}

/* The power-up wait and stop-before-start are chip constraints: the SEN55
 * needs SEN55_POWER_UP_MS after its rail comes up, and both chips keep
 * measuring across a soft reboot while only accepting start from idle. */
static void sensors_init() {
    esp_err_t err = bsp_power_set_switch(BSP_POWER_SWITCH_SENSOR, true);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "sensor power switch: %s", esp_err_to_name(err));
        return;
    }
    vTaskDelay(pdMS_TO_TICKS(SEN55_POWER_UP_MS));

    i2c_master_bus_handle_t bus = bsp_bus_get_i2c_handle(I2C_NUM_0);
    if (!bus) return;

    sen55_config_t sen55_cfg = {};
    sen55_cfg.i2c_bus = bus;
    if (sen55_create(&sen55_cfg, &s_sen55) == ESP_OK) {
        sen55_stop(s_sen55);
        sen55_start(s_sen55);
        ESP_LOGI(TAG, "SEN55 measuring");
    } else {
        ESP_LOGW(TAG, "SEN55 unavailable");
    }

    scd40_config_t scd40_cfg = {};
    scd40_cfg.i2c_bus = bus;
    if (scd40_create(&scd40_cfg, &s_scd40) == ESP_OK) {
        scd40_stop(s_scd40);
        scd40_start(s_scd40);
        ESP_LOGI(TAG, "SCD40 measuring");
    } else {
        ESP_LOGW(TAG, "SCD40 unavailable");
    }
}

static void append_line(char *buf, size_t size, const char *label, float value,
                        int decimals, const char *unit) {
    size_t len = strlen(buf);
    if (std::isnan(value)) {
        snprintf(buf + len, size - len, "%s --\n", label);
    } else {
        snprintf(buf + len, size - len, "%s %.*f%s\n", label, decimals, value, unit);
    }
}

static void render_air() {
    char text[160] = "";
    append_line(text, sizeof(text), "CO2:", s_co2_valid ? (float)s_co2.co2_ppm : NAN, 0, " ppm");
    append_line(text, sizeof(text), "PM2.5:", s_air_valid ? s_air.pm2_5 : NAN, 1, " ug/m3");
    append_line(text, sizeof(text), "Temp:", s_air_valid ? s_air.temperature : NAN, 1, " C");
    append_line(text, sizeof(text), "Hum:", s_air_valid ? s_air.humidity : NAN, 0, " %");
    append_line(text, sizeof(text), "VOC:", s_air_valid ? s_air.voc_index : NAN, 0, "");
    lv_label_set_text(s_air_label, text);
    s_refresh_mode = BSP_EPD_MODE_FAST;
}

/* Runs on the LVGL task. Renders when the SCD40 delivers (5 s), or every 5th
 * SEN55 sample when the SCD40 is absent, keeping EPD refreshes sparse. */
static void poll_sensors() {
    static uint8_t air_pending = 0;
    bool render = false;
    bool ready = false;
    if (s_sen55 && sen55_data_ready(s_sen55, &ready) == ESP_OK && ready &&
        sen55_read(s_sen55, &s_air) == ESP_OK) {
        s_air_valid = true;
        air_pending++;
    }
    ready = false;
    if (s_scd40 && scd40_data_ready(s_scd40, &ready) == ESP_OK && ready &&
        scd40_read(s_scd40, &s_co2) == ESP_OK) {
        s_co2_valid = true;
        render = true;
    }
    if (render || air_pending >= 5) {
        air_pending = 0;
        render_air();
    }
}

static int s_count = 0;

static void render_count() {
    char text[24];
    snprintf(text, sizeof(text), "Count: %d", s_count);
    lv_label_set_text(s_label, text);
    s_refresh_mode = BSP_EPD_MODE_FAST;
    ESP_LOGI(TAG, "partial refresh -> %d", s_count);
}

/* Buttons fire on the shared BSP dispatch task; marshal the LVGL/refresh work
 * onto the LVGL context. Each press drives a fast partial refresh of the
 * counter. */
static void bump_counter(int delta) {
    bsp_audio_tone(delta > 0 ? 2000 : 1000, 60);
    lv_async_call([delta] {
        s_count += delta;
        render_count();
    });
}

static void reset_counter() {
    bsp_audio_tone(1500, 60);
    lv_async_call([] {
        s_count = 0;
        render_count();
    });
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.epd.task_priority = 5;
    bsp_config.epd.task_affinity = 0;
    bsp_init(&bsp_config);
    lvgl_init();
    sensors_init();

    bsp_button_on_click(0, [](uint8_t, void *) { bump_counter(+1); }, nullptr);   /* A: top */
    bsp_button_on_click(1, [](uint8_t, void *) { bump_counter(-1); }, nullptr);   /* B */
    bsp_button_on_click(2, [](uint8_t, void *) { reset_counter(); }, nullptr);    /* power: reset */

    lv_async_call([] {
        build_hello_screen();                       /* first frame: full refresh */
        lv_timer_create([](lv_timer_t *) { poll_sensors(); }, 1000, nullptr);
    });
}
