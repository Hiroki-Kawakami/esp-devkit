/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal M5Stack CoreS3 sample: brings up the BSP (AXP2101 rails + AW9523B reset
 * + ILI9342C panel + FT6336U touch) and LVGL, then shows a title, a once-a-second
 * counter, the AXP2101 battery voltage and the last touch coordinate -- enough to
 * prove the display + PMIC + touch path end to end. The panel has no host
 * framebuffer, so LVGL renders into partial draw buffers that flush through
 * bsp_display_draw_bitmap; touch is read through bsp_touch_read.
 */

#include "core_s3_hello.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include <assert.h>

static const char *TAG = "core_s3_hello";

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

    /* Pointer indev fed from the polled FT6336U (bsp_touch_read = latest frame). */
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

/* ---- AW88298 speaker test ----------------------------------------------- */

static int s_volume = 70;

static lv_obj_t *make_button(lv_obj_t *parent, const char *text) {
    lv_obj_t *btn = lv_button_create(parent);
    lv_obj_set_style_pad_all(btn, 10, 0);
    lv_obj_t *lbl = lv_label_create(btn);
    lv_label_set_text(lbl, text);
    lv_obj_center(lbl);
    return btn;
}

/* Sustained tone so volume changes are audible while it plays. */
static void tone_toggle_cb(lv_event_t *e) {
    lv_obj_t *btn = lv_event_get_target_obj(e);
    if (lv_obj_has_state(btn, LV_STATE_CHECKED)) bsp_audio_tone(440, 0);
    else                                         bsp_audio_tone_stop();
}

struct vol_btn { lv_obj_t *label; int delta; };
static void volume_cb(lv_event_t *e) {
    auto *c = static_cast<vol_btn *>(lv_event_get_user_data(e));
    s_volume += c->delta;
    if (s_volume < 0)   s_volume = 0;
    if (s_volume > 100) s_volume = 100;
    bsp_audio_set_volume(s_volume);
    lv_label_set_text_fmt(c->label, "Vol: %d", s_volume);
}

static void build_audio_test(lv_obj_t *scr) {
    if (!(bsp_audio_get_caps() & BSP_AUDIO_CAP_SPEAKER)) return;

    lv_obj_t *vol = lv_label_create(scr);
    lv_obj_set_style_text_font(vol, &lv_font_montserrat_16, 0);
    lv_obj_set_style_text_color(vol, lv_color_hex(0x40C0FF), 0);
    lv_label_set_text_fmt(vol, "Vol: %d", s_volume);
    bsp_audio_set_volume(s_volume);

    lv_obj_t *row = lv_obj_create(scr);
    lv_obj_set_size(row, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_obj_set_style_bg_opa(row, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(row, 0, 0);
    lv_obj_set_style_pad_all(row, 0, 0);
    lv_obj_set_style_pad_column(row, 8, 0);
    lv_obj_set_flex_flow(row, LV_FLEX_FLOW_ROW);
    lv_obj_clear_flag(row, LV_OBJ_FLAG_SCROLLABLE);

    lv_obj_add_event_cb(make_button(row, "-"), volume_cb, LV_EVENT_CLICKED, new vol_btn{ vol, -10 });
    lv_obj_add_event_cb(make_button(row, "+"), volume_cb, LV_EVENT_CLICKED, new vol_btn{ vol, +10 });

    lv_obj_t *tone = make_button(row, "TONE");
    lv_obj_add_flag(tone, LV_OBJ_FLAG_CHECKABLE);
    lv_obj_add_event_cb(tone, tone_toggle_cb, LV_EVENT_VALUE_CHANGED, nullptr);
}

static void build_hello_screen() {
    lv_obj_t *scr = lv_screen_active();
    lv_obj_set_style_bg_color(scr, lv_color_black(), 0);
    lv_obj_set_flex_flow(scr, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(scr, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_row(scr, 16, 0);

    lv_obj_t *title = lv_label_create(scr);
    lv_label_set_text(title, "CoreS3");
    lv_obj_set_style_text_font(title, &lv_font_montserrat_32, 0);
    lv_obj_set_style_text_color(title, lv_color_hex(0x00E0A0), 0);

    lv_obj_t *counter = lv_label_create(scr);
    lv_obj_set_style_text_font(counter, &lv_font_montserrat_24, 0);
    lv_obj_set_style_text_color(counter, lv_color_white(), 0);
    lv_label_set_text(counter, "0 s");

    lv_obj_t *battery = lv_label_create(scr);
    lv_obj_set_style_text_font(battery, &lv_font_montserrat_16, 0);
    lv_obj_set_style_text_color(battery, lv_color_hex(0x808080), 0);
    lv_label_set_text(battery, "-- mV");

    lv_obj_t *touch = lv_label_create(scr);
    lv_obj_set_style_text_font(touch, &lv_font_montserrat_16, 0);
    lv_obj_set_style_text_color(touch, lv_color_hex(0xF0C000), 0);
    lv_label_set_text(touch, "touch: --");

    /* Once a second: proves the render/flush loop and reads the PMIC. */
    struct tick_ctx { lv_obj_t *counter; lv_obj_t *battery; int secs; };
    auto *ctx = new tick_ctx{ counter, battery, 0 };
    lv_timer_create([](lv_timer_t *t) {
        auto *c = static_cast<tick_ctx *>(lv_timer_get_user_data(t));
        lv_label_set_text_fmt(c->counter, "%d s", ++c->secs);
        uint32_t mv = 0;
        if (bsp_power_get_battery_voltage(&mv) == ESP_OK)
            lv_label_set_text_fmt(c->battery, "%lu mV", (unsigned long)mv);
    }, 1000, ctx);

    /* Report the latest FT6336U contact so touch is visibly working. */
    lv_timer_create([](lv_timer_t *t) {
        auto *label = static_cast<lv_obj_t *>(lv_timer_get_user_data(t));
        bsp_touch_point_t pt;
        if (bsp_touch_read(&pt, 1) > 0) lv_label_set_text_fmt(label, "touch: %d,%d", pt.x, pt.y);
        else                            lv_label_set_text(label, "touch: --");
    }, 50, touch);

    build_audio_test(scr);
}

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.dispatch.task_priority = 6;
    bsp_init(&bsp_config);
    bsp_display_set_brightness(100);
    lvgl_init();

    lv_async_call([] {
        build_hello_screen();
    });
}
