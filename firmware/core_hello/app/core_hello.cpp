/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal M5Stack Core sample: brings up the BSP + LVGL on the 320x240 ILI9342C
 * panel and shows a title and a once-a-second counter -- enough to prove the
 * display path end to end. The panel has no host framebuffer, so LVGL renders
 * into partial draw buffers that flush through bsp_display_draw_bitmap.
 */

#include "core_hello.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "screens/home_screen.hpp"
#include "screens/lcd_test_screen.hpp"
#include "screens/input_test_screen.hpp"
#include <assert.h>

static const char *TAG = "core_hello";

static void flush_cb(lv_display_t *disp, const lv_area_t *area, uint8_t *px_map) {
    const bsp_rect_t rect = {
        { area->x1, area->y1 },
        { area->x2 - area->x1 + 1, area->y2 - area->y1 + 1 },
    };
    bsp_display_draw_bitmap(rect, px_map, BSP_ROTATION_0);
    lv_display_flush_ready(disp);
}

static void lvgl_init() {
    const lvgl_port_cfg_t config = {
        .task_priority     = 4,
        .task_stack        = 8192,
        .task_affinity     = -1,
        .task_max_sleep_ms = 500,
        .task_stack_caps   = MALLOC_CAP_INTERNAL | MALLOC_CAP_DEFAULT,
        .timer_period_ms   = 5,
    };
    esp_err_t err = lvgl_port_init(&config);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "lvgl_port_init: %s", esp_err_to_name(err));
        assert(0);
    }

    const bsp_size_t size = bsp_display_get_size();

    /* SPI panel: no host framebuffer, so render into two quarter-screen partial
     * buffers and blit each flushed region over draw_bitmap. */
    const int    lines    = size.height / 4;
    const size_t buf_bytes = (size_t)size.width * lines * sizeof(uint16_t);
    void *buf0 = heap_caps_malloc(buf_bytes, MALLOC_CAP_DEFAULT);
    void *buf1 = heap_caps_malloc(buf_bytes, MALLOC_CAP_DEFAULT);
    assert(buf0 && buf1);

    lv_display_t *disp = lv_display_create(size.width, size.height);
    lv_display_set_color_format(disp, LV_COLOR_FORMAT_RGB565);
    lv_display_set_buffers(disp, buf0, buf1, buf_bytes, LV_DISPLAY_RENDER_MODE_PARTIAL);
    lv_display_set_flush_cb(disp, flush_cb);
    lv_display_set_default(disp);
}

static std::shared_ptr<Screen> make_screen(int index) {
    switch (index) {
        case 1:  return std::make_shared<LcdTestScreen>();
        case 2:  return std::make_shared<InputTestScreen>();
        default: return std::make_shared<HomeScreen>();
    }
}

static int s_screen_index = 0;

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.dispatch.task_priority = 6;
    bsp_init(&bsp_config);
    bsp_display_set_brightness(100);
    lvgl_init();

    bsp_button_on_click(2, [](uint8_t, void *) {
        lv_async_call([] {
            s_screen_index = (s_screen_index + 1) % 3;
            screen_manager.load(make_screen(s_screen_index));
        });
    }, nullptr);

    lv_async_call([] {
        screen_manager.load(make_screen(s_screen_index));
    });
}
