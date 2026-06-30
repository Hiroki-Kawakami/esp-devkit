/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal Tab5 sample: brings up the BSP + LVGL and shows a centered label.
 */

#include "tab5_hello.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include <assert.h>

static const char *TAG = "tab5_hello";

static void lvgl_init() {
    lvgl_port_cfg_t config = {
        .task_priority    = 4,
        .task_stack       = 8192,
        .task_affinity    = 1,
        .task_max_sleep_ms = 500,
        .task_stack_caps  = MALLOC_CAP_INTERNAL | MALLOC_CAP_DEFAULT,
        .timer_period_ms  = 5,
    };
    esp_err_t err = lvgl_port_init(&config);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "lvgl_port_init: %s", esp_err_to_name(err));
        assert(0);
    }

    const bsp_size_t         size = bsp_display_get_size();
    const bsp_pixel_format_t fmt  = bsp_display_get_pixel_format();
    void  *fb0 = bsp_display_get_frame_buffer(0);
    void  *fb1 = bsp_display_get_frame_buffer(1);
    const size_t fb_bytes = (size_t)size.width * size.height * bsp_pixel_format_bytes(fmt);

    lv_display_t *disp = lv_display_create(size.width, size.height);
    lv_display_set_color_format(disp,
        fmt == BSP_PIXEL_FORMAT_RGB888 ? LV_COLOR_FORMAT_RGB888 : LV_COLOR_FORMAT_RGB565);
    lv_display_set_buffers(disp, fb0, fb1, fb_bytes, LV_DISPLAY_RENDER_MODE_DIRECT);
    lv_display_set_flush_cb(disp, [](lv_display_t *d, const lv_area_t *, uint8_t *px_map) {
        int fb_index = (px_map == bsp_display_get_frame_buffer(1)) ? 1 : 0;
        bsp_display_flush(fb_index);
        lv_display_flush_ready(d);
    });
    lv_display_set_default(disp);
}

static void build_hello_screen() {
    lv_obj_t *scr = lv_screen_active();
    lv_obj_set_style_bg_color(scr, lv_color_white(), 0);

    lv_obj_t *label = lv_label_create(scr);
    lv_label_set_text(label, "Hello, Tab5!");
    lv_obj_set_style_text_font(label, &lv_font_montserrat_48, 0);
    lv_obj_set_style_text_color(label, lv_color_black(), 0);
    lv_obj_center(label);
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.touch.task_priority = 6;
    bsp_config.touch.task_affinity = 1;
    bsp_init(&bsp_config);
    lvgl_init();

    lv_async_call([] {
        build_hello_screen();
        bsp_display_set_brightness(80);
    });
}
