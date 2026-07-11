/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "home_screen.hpp"

void HomeScreen::build() {
    lv_obj_set_style_bg_color(root_, lv_color_black(), 0);
    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(root_, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_row(root_, 16, 0);

    lv_obj_t *title = lv_label_create(root_);
    lv_label_set_text(title, "M5Stack Core");
    lv_obj_set_style_text_font(title, &lv_font_montserrat_32, 0);
    lv_obj_set_style_text_color(title, lv_color_hex(0x00E0A0), 0);

    counter_ = lv_label_create(root_);
    lv_obj_set_style_text_font(counter_, &lv_font_montserrat_24, 0);
    lv_obj_set_style_text_color(counter_, lv_color_white(), 0);
    lv_label_set_text(counter_, "0 s");

    lv_obj_t *hint = lv_label_create(root_);
    lv_obj_set_style_text_color(hint, lv_color_hex(0x808080), 0);
    lv_label_set_text(hint, "BtnC: switch screen");
}

void HomeScreen::onAppear() {
    timer_ = lv_timer_create([](lv_timer_t *t) {
        auto *c = static_cast<HomeScreen*>(lv_timer_get_user_data(t));
        lv_label_set_text_fmt(c->counter_, "%d s", ++c->count_);
    }, 1000, this);
}

void HomeScreen::onDisappear() {
    lv_timer_del(timer_);
}
