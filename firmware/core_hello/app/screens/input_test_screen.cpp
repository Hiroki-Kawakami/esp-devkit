/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "input_test_screen.hpp"
#include <array>
#include <cstdio>

namespace {

InputTestScreen *g_active = nullptr;
constexpr size_t MAX_LOG_LINES = 8;

const char *btn_name(uint8_t id) {
    switch (id) {
        case 0:  return "BtnA";
        case 1:  return "BtnB";
        case 2:  return "BtnC";
        default: return "Btn?";
    }
}

}

void InputTestScreen::build() {
    lv_obj_set_style_bg_color(root_, lv_color_black(), 0);
    lv_obj_set_style_pad_all(root_, 8, 0);
    lv_obj_set_style_pad_row(root_, 6, 0);
    lv_obj_remove_flag(root_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);

    lv_obj_t *title = lv_label_create(root_);
    lv_label_set_text(title, "Input Test");
    lv_obj_set_style_text_font(title, &lv_font_montserrat_24, 0);
    lv_obj_set_style_text_color(title, lv_color_hex(0x00E0A0), 0);

    touch_label_ = lv_label_create(root_);
    lv_obj_set_style_text_color(touch_label_, lv_color_white(), 0);
    lv_label_set_text(touch_label_, "Touch: --");

    log_label_ = lv_label_create(root_);
    lv_obj_set_style_text_color(log_label_, lv_color_hex(0xB0B0B0), 0);
    lv_label_set_text(log_label_, "");
}

void InputTestScreen::onAppear() {
    g_active = this;

    bsp_touch_set_event_cb([](const bsp_touch_point_t *points, int count, void *) {
        std::array<bsp_touch_point_t, 5> buf{};
        int n = count < 0 ? 0 : (count > (int)buf.size() ? (int)buf.size() : count);
        for (int i = 0; i < n; i++) buf[i] = points[i];
        lv_async_call([buf, n] { if (g_active) g_active->on_touch(buf.data(), n); });
    }, nullptr);

    uint8_t count = bsp_button_count();
    for (uint8_t id = 0; id < count; id++) {
        bsp_button_on_down(id, [](uint8_t id, void *) {
            lv_async_call([id] { if (g_active) g_active->on_button(id, "DOWN"); });
        }, nullptr);
        bsp_button_on_up(id, [](uint8_t id, void *) {
            lv_async_call([id] { if (g_active) g_active->on_button(id, "UP"); });
        }, nullptr);
        if (id != 2) {
            bsp_button_on_click(id, [](uint8_t id, void *) {
                lv_async_call([id] { if (g_active) g_active->on_button(id, "CLICK"); });
            }, nullptr);
        }
    }
}

void InputTestScreen::onDisappear() {
    bsp_touch_set_event_cb(nullptr, nullptr);
    uint8_t count = bsp_button_count();
    for (uint8_t id = 0; id < count; id++) {
        bsp_button_on_down(id, nullptr, nullptr);
        bsp_button_on_up(id, nullptr, nullptr);
        if (id != 2) bsp_button_on_click(id, nullptr, nullptr);
    }
    g_active = nullptr;
}

void InputTestScreen::on_touch(const bsp_touch_point_t *points, int count) {
    if (count > 0) {
        lv_label_set_text_fmt(touch_label_, "Touch: (%d, %d)  x%d", points[0].x, points[0].y, count);
    } else {
        lv_label_set_text(touch_label_, "Touch: --");
    }
    if (count > 0 && prev_touch_count_ == 0)      add_event("TOUCH down");
    else if (count == 0 && prev_touch_count_ > 0) add_event("TOUCH up");
    prev_touch_count_ = count;
}

void InputTestScreen::on_button(uint8_t id, const char *event) {
    char line[24];
    snprintf(line, sizeof line, "%s %s", btn_name(id), event);
    add_event(line);
}

void InputTestScreen::add_event(const char *text) {
    events_.push_back(text);
    if (events_.size() > MAX_LOG_LINES) events_.erase(events_.begin());
    render_log();
}

void InputTestScreen::render_log() {
    std::string text;
    for (size_t i = 0; i < events_.size(); i++) {
        if (i) text += '\n';
        text += events_[i];
    }
    lv_label_set_text(log_label_, text.c_str());
}
