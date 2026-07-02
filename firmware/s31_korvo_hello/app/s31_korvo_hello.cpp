/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal ESP32-S31-Korvo sample: brings up the BSP + LVGL, shows a title and a
 * button that counts taps. Also wires bsp_button callbacks for the ADC ladder
 * (VOL-UP / VOL-DOWN / MODE / SET) so button events get printed to the log.
 */

#include "s31_korvo_hello.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include <assert.h>

static const char *TAG = "s31_korvo_hello";

static struct {
    volatile int  x;
    volatile int  y;
    volatile bool pressed;
} s_touch;

static void on_touch_push(const bsp_touch_point_t *pts, int count, void *) {
    if (count > 0) {
        s_touch.x = pts[0].x;
        s_touch.y = pts[0].y;
        s_touch.pressed = true;
    } else {
        s_touch.pressed = false;
    }
}

static void indev_read(lv_indev_t *, lv_indev_data_t *data) {
    if (s_touch.pressed) {
        data->point.x = s_touch.x;
        data->point.y = s_touch.y;
        data->state   = LV_INDEV_STATE_PRESSED;
    } else {
        data->state   = LV_INDEV_STATE_RELEASED;
    }
}

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

    lv_indev_t *indev = lv_indev_create();
    lv_indev_set_type(indev, LV_INDEV_TYPE_POINTER);
    lv_indev_set_read_cb(indev, indev_read);
    lv_indev_set_display(indev, disp);
}

static void build_hello_screen() {
    lv_obj_t *scr = lv_screen_active();
    lv_obj_set_style_bg_color(scr, lv_color_white(), 0);
    lv_obj_set_flex_flow(scr, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(scr, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_row(scr, 24, 0);

    lv_obj_t *title = lv_label_create(scr);
    lv_label_set_text(title, "Hello, S31-Korvo!");
    lv_obj_set_style_text_font(title, &lv_font_montserrat_48, 0);
    lv_obj_set_style_text_color(title, lv_color_black(), 0);

    lv_obj_t *counter = lv_label_create(scr);
    lv_label_set_text(counter, "Taps: 0");
    lv_obj_set_style_text_font(counter, &lv_font_montserrat_32, 0);
    lv_obj_set_style_text_color(counter, lv_color_black(), 0);

    lv_obj_t *btn = lv_button_create(scr);
    lv_obj_set_size(btn, 260, 90);
    lv_obj_add_event_fn(btn, LV_EVENT_CLICKED, [counter](lv_event_t *) {
        static int taps = 0;
        lv_label_set_text_fmt(counter, "Taps: %d", ++taps);
        /* R -> G -> B -> off, then repeat. No-op on hosts without an LED provider. */
        static const uint8_t palette[4][3] = {
            {0x40, 0x00, 0x00}, {0x00, 0x40, 0x00}, {0x00, 0x00, 0x40}, {0, 0, 0},
        };
        const uint8_t *c = palette[taps % 4];
        bsp_led_set_rgb(0, c[0], c[1], c[2]);
        ESP_LOGI(TAG, "tap %d", taps);
    });

    lv_obj_t *btn_label = lv_label_create(btn);
    lv_label_set_text(btn_label, "Tap me");
    lv_obj_set_style_text_font(btn_label, &lv_font_montserrat_32, 0);
    lv_obj_center(btn_label);
}

/* Physical buttons on the ADC ladder. Labels match the S31-Korvo silkscreen. */
enum { BTN_VOLUP = 0, BTN_VOLDOWN = 1, BTN_MODE = 2, BTN_SET = 3 };

static void wire_button_test() {
    static const char *label[] = { "VOL-UP", "VOL-DOWN", "MODE", "SET" };

    /* VOL-UP: exercise click / double-click / long-press together. Registering
     * on_double_click delays the single click by 300 ms so a fast second press
     * fires DOUBLE instead. */
    bsp_button_on_click(BTN_VOLUP, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: click", label[id]);
    }, nullptr);
    bsp_button_on_double_click(BTN_VOLUP, 300, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: DOUBLE-click", label[id]);
    }, nullptr);
    bsp_button_on_long_press(BTN_VOLUP, 800, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: LONG-press", label[id]);
    }, nullptr);

    /* VOL-DOWN: raw down/up edges only. */
    bsp_button_on_down(BTN_VOLDOWN, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: down", label[id]);
    }, nullptr);
    bsp_button_on_up(BTN_VOLDOWN, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: up", label[id]);
    }, nullptr);

    /* MODE / SET: simple click. */
    bsp_button_on_click(BTN_MODE, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: click", label[id]);
    }, nullptr);
    bsp_button_on_click(BTN_SET, [](uint8_t id, void *) {
        ESP_LOGI(TAG, "%s: click", label[id]);
    }, nullptr);
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.touch.task_priority = 6;
    bsp_config.touch.task_affinity = 1;
    bsp_init(&bsp_config);
    bsp_touch_set_event_cb(on_touch_push, nullptr);
    lvgl_init();
    wire_button_test();

    lv_async_call([] {
        build_hello_screen();
    });
}
