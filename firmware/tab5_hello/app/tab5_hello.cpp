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

}

/* Optional HDMI module on the M-Bus: absent hardware just leaves the internal
 * panel as the only display. */
static void hdmi_init() {
    bsp_panel_id_t panel = BSP_PANEL_MAIN;
    esp_err_t err = bsp_module_attach_display(BSP_MODULE_PORT_MBUS, nullptr, &panel);
    if (err != ESP_OK) {
        ESP_LOGI(TAG, "display module not attached: %s", esp_err_to_name(err));
        return;
    }

    lv_async_call([panel] {
        DisplayManagerConfig display_config = {};
        display_config.viewport.panel = panel;
        display_config.buffer.lines = 32;
        display_config.make_default = false;
        lv_display_t *disp = nullptr;
        esp_err_t err = display_manager.create_display(display_config, &disp);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "create_display(hdmi): %s", esp_err_to_name(err));
            return;
        }

        lv_obj_t *screen = lv_display_get_screen_active(disp);
        lv_obj_set_style_bg_color(screen, lv_color_hex(0x102040), LV_PART_MAIN);
        lv_obj_t *label = lv_label_create(screen);
        lv_label_set_text(label, "esp-devkit / Tab5 HDMI");
        lv_obj_set_style_text_color(label, lv_color_white(), LV_PART_MAIN);
        lv_obj_center(label);
    });
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.display.pixel_format = BSP_PIXEL_FORMAT_RGB565;
    bsp_config.dispatch.task_priority = 6;
    bsp_config.dispatch.task_affinity = 1;
    bsp_init(&bsp_config);
    lvgl_init();
    hdmi_init();

    lv_async_call([] {
        screen_manager.load(std::make_shared<HomeScreen>());
        bsp_display_set_brightness(BSP_PANEL_MAIN, 80);
    });
}
