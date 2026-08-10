/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal Tab5 sample: brings up the BSP + LVGL and shows a centered label.
 */

#include "tab5_hello.hpp"
#include "display_manager.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "screens/home_screen.hpp"

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
        return;
    }

    DisplayManagerConfig display_config = {};
    lv_display_t *disp = nullptr;
    err = display_manager.create_display(display_config, &disp);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "create_display: %s", esp_err_to_name(err));
        return;
    }

    lv_indev_t *indev = lv_indev_create();
    lv_indev_set_type(indev, LV_INDEV_TYPE_POINTER);
    lv_indev_set_read_cb(indev, [](lv_indev_t *, lv_indev_data_t *data) {
        bsp_touch_point_t pt;
        if (bsp_touch_read(&pt, 1) > 0) {
            data->point.x = pt.x;
            data->point.y = pt.y;
            data->state   = LV_INDEV_STATE_PRESSED;
        } else {
            data->state = LV_INDEV_STATE_RELEASED;
        }
    });
    lv_indev_set_display(indev, disp);
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.display.pixel_format = BSP_PIXEL_FORMAT_RGB565;
    bsp_config.dispatch.task_priority = 6;
    bsp_config.dispatch.task_affinity = 1;
    bsp_init(&bsp_config);
    lvgl_init();

    lv_async_call([] {
        screen_manager.load(std::make_shared<HomeScreen>());
        bsp_display_set_brightness(80);
    });
}
