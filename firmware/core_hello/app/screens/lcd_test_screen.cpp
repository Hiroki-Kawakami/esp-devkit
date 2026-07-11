/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "lcd_test_screen.hpp"
#include "bsp.h"

namespace { LcdTestScreen *g_active = nullptr; }

void LcdTestScreen::build() {
    lv_obj_set_style_bg_color(root_, lv_color_black(), 0);
    lv_obj_set_style_pad_all(root_, 0, 0);
    lv_obj_remove_flag(root_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);

    lv_obj_t *strip = lv_obj_create(root_);
    lv_obj_remove_style_all(strip);
    lv_obj_remove_flag(strip, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_width(strip, LV_PCT(100));
    lv_obj_set_flex_grow(strip, 1);
    lv_obj_set_flex_flow(strip, LV_FLEX_FLOW_ROW);

    static const uint32_t colors[] = {
        0xFFFFFF, 0xFFFF00, 0x00FFFF, 0x00FF00,
        0xFF00FF, 0xFF0000, 0x0000FF, 0x000000,
    };
    for (uint32_t c : colors) {
        lv_obj_t *bar = lv_obj_create(strip);
        lv_obj_remove_style_all(bar);
        lv_obj_set_height(bar, LV_PCT(100));
        lv_obj_set_flex_grow(bar, 1);
        lv_obj_set_style_bg_color(bar, lv_color_hex(c), 0);
        lv_obj_set_style_bg_opa(bar, LV_OPA_COVER, 0);
    }

    brightness_label_ = lv_label_create(root_);
    lv_obj_set_width(brightness_label_, LV_PCT(100));
    lv_obj_set_style_bg_color(brightness_label_, lv_color_black(), 0);
    lv_obj_set_style_bg_opa(brightness_label_, LV_OPA_COVER, 0);
    lv_obj_set_style_text_color(brightness_label_, lv_color_white(), 0);
    lv_obj_set_style_text_align(brightness_label_, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_set_style_pad_ver(brightness_label_, 6, 0);
}

void LcdTestScreen::onAppear() {
    g_active = this;
    apply();
    bsp_button_on_click(0, [](uint8_t, void *) {
        lv_async_call([] { if (g_active) g_active->adjust(-10); });
    }, nullptr);
    bsp_button_on_click(1, [](uint8_t, void *) {
        lv_async_call([] { if (g_active) g_active->adjust(+10); });
    }, nullptr);
}

void LcdTestScreen::onDisappear() {
    bsp_button_on_click(0, nullptr, nullptr);
    bsp_button_on_click(1, nullptr, nullptr);
    g_active = nullptr;
}

void LcdTestScreen::adjust(int delta) {
    brightness_ += delta;
    if (brightness_ < 0)   brightness_ = 0;
    if (brightness_ > 100) brightness_ = 100;
    apply();
}

void LcdTestScreen::apply() {
    bsp_display_set_brightness(brightness_);
    lv_label_set_text_fmt(brightness_label_, "Brightness: %d%%   (BtnA -  BtnB +)", brightness_);
}
