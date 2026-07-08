/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal M5 Air Quality Kit sample: brings up the BSP + LVGL and shows a
 * centered counter on the GDEY0154D67 EPD. The EPD has no host framebuffer, so
 * LVGL renders into its own L8 buffer and flush blits each region into the panel
 * buffer; the last flush of a render drives the whole panel with s_refresh_mode.
 *
 * Partial-refresh test: the first frame is a full refresh (seeds the panel base
 * image), then the two front buttons drive the counter -- A (top) increments,
 * B decrements -- each as a fast direct-update (BSP_EPD_MODE_FAST) partial
 * refresh.
 */

#include "airq_hello.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include <assert.h>
#include <cstdio>

static const char *TAG = "airq_hello";

static lv_display_t  *s_disp;
static uint8_t       *s_buf;
static lv_obj_t      *s_label;
static bsp_epd_mode_t s_refresh_mode = BSP_EPD_MODE_QUALITY;

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
    lv_obj_center(s_label);
}

/* Buttons fire on the shared BSP dispatch task; marshal the LVGL/refresh work
 * onto the LVGL context. Each press drives a fast partial refresh of the
 * counter. */
static void bump_counter(int delta) {
    lv_async_call([delta] {
        static int n = 0;
        n += delta;

        char text[24];
        snprintf(text, sizeof(text), "Count: %d", n);
        lv_label_set_text(s_label, text);
        s_refresh_mode = BSP_EPD_MODE_FAST;
        ESP_LOGI(TAG, "partial refresh -> %d", n);
    });
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.epd.task_priority = 5;
    bsp_config.epd.task_affinity = 0;
    bsp_init(&bsp_config);
    lvgl_init();

    bsp_button_on_click(0, [](uint8_t, void *) { bump_counter(+1); }, nullptr);   /* A: top */
    bsp_button_on_click(1, [](uint8_t, void *) { bump_counter(-1); }, nullptr);   /* B */

    lv_async_call([] {
        build_hello_screen();                       /* first frame: full refresh */
    });
}
