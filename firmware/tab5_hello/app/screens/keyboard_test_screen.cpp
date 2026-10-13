/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "keyboard_test_screen.hpp"
#include "esp_log.h"
#include <cstdio>

static const char *TAG = "keyboard_test";

static constexpr size_t kLogLines = 16;

static const char *mode_name(bsp_keyboard_mode_t mode) {
    switch (mode) {
        case BSP_KEYBOARD_MODE_HID:  return "HID";
        case BSP_KEYBOARD_MODE_RAW:  return "RAW";
        case BSP_KEYBOARD_MODE_CHAR: return "CHAR";
        default:                     return "?";
    }
}

static lv_obj_t *add_row(lv_obj_t *parent) {
    auto row = lv_obj_create(parent);
    lv_obj_remove_style_all(row);
    lv_obj_set_size(row, LV_PCT(100), LV_SIZE_CONTENT);
    lv_obj_set_flex_flow(row, LV_FLEX_FLOW_ROW);
    lv_obj_set_style_pad_column(row, lv_obj_get_style_pad_row(parent, LV_PART_MAIN), 0);
    return row;
}

static lv_obj_t *add_button(lv_obj_t *row, const char *text) {
    auto button = lv_button_create(row);
    lv_obj_set_flex_grow(button, 1);
    auto title = lv_label_create(button);
    lv_obj_center(title);
    lv_label_set_text(title, text);
    return button;
}

void KeyboardTestScreen::build() {
    createNavigation("Keyboard Test");

    status_ = lv_label_create(contents_);
    lv_obj_set_style_text_color(status_, lv_color_black(), 0);

    auto modes = add_row(contents_);
    for (auto mode : { BSP_KEYBOARD_MODE_HID, BSP_KEYBOARD_MODE_RAW, BSP_KEYBOARD_MODE_CHAR }) {
        auto button = add_button(modes, mode_name(mode));
        lv_obj_add_event_fn(button, LV_EVENT_CLICKED, [this, mode](lv_event_t *) {
            setMode(mode);
        });
    }

    auto actions = add_row(contents_);
    auto probe_button = add_button(actions, "Probe");
    lv_obj_add_event_fn(probe_button, LV_EVENT_CLICKED, [this](lv_event_t *) {
        probe();
    });
    auto clear_button = add_button(actions, "Clear Log");
    lv_obj_add_event_fn(clear_button, LV_EVENT_CLICKED, [this](lv_event_t *) {
        lines_.clear();
        lv_label_set_text(log_, "");
    });

    struct LedColor { const char *name; uint8_t r, g, b; };
    static constexpr LedColor kColors[] = {
        { "LED Red", 255, 0, 0 }, { "LED Green", 0, 255, 0 },
        { "LED Blue", 0, 0, 255 }, { "LED Off", 0, 0, 0 },
    };
    auto leds = add_row(contents_);
    for (const auto &color : kColors) {
        auto button = add_button(leds, color.name);
        lv_obj_add_event_fn(button, LV_EVENT_CLICKED, [this, color](lv_event_t *) {
            setLeds(color.r, color.g, color.b);
        });
    }

    log_ = lv_label_create(contents_);
    lv_obj_set_width(log_, LV_PCT(100));
    lv_obj_set_style_text_color(log_, lv_color_black(), 0);
    lv_label_set_text(log_, "");

    updateStatus();
}

void KeyboardTestScreen::onEnter() {
    self_ = shared_from_this();
    bsp_keyboard_set_event_cb([](const bsp_keyboard_event_t *event, void *arg) {
        auto weak = *static_cast<std::weak_ptr<Screen> *>(arg);
        bsp_keyboard_event_t copy = *event;
        lv_async_call([weak, copy] {
            if (auto screen = weak.lock()) {
                std::static_pointer_cast<KeyboardTestScreen>(screen)->handleEvent(copy);
            }
        });
    }, &self_);
}

void KeyboardTestScreen::onExit() {
    bsp_keyboard_set_event_cb(nullptr, nullptr);
}

void KeyboardTestScreen::setMode(bsp_keyboard_mode_t mode) {
    esp_err_t err = bsp_keyboard_set_mode(mode);
    append(std::string("set_mode ") + mode_name(mode) + ": " + esp_err_to_name(err));
    updateStatus();
}

void KeyboardTestScreen::probe() {
    esp_err_t err = bsp_keyboard_probe();
    append(std::string("probe: ") + esp_err_to_name(err));
    updateStatus();
}

void KeyboardTestScreen::setLeds(uint8_t r, uint8_t g, uint8_t b) {
    esp_err_t err = ESP_OK;
    for (uint8_t i = 0; i < bsp_led_count() && err == ESP_OK; i++) {
        err = bsp_led_set_rgb(i, r, g, b);
    }
    char line[64];
    snprintf(line, sizeof(line), "led x%u #%02X%02X%02X: %s",
             bsp_led_count(), r, g, b, esp_err_to_name(err));
    append(line);
}

void KeyboardTestScreen::handleEvent(const bsp_keyboard_event_t &event) {
    char line[64];
    switch (event.type) {
        case BSP_KEYBOARD_EVENT_HID:
            snprintf(line, sizeof(line), "HID mod=0x%02X key=0x%02X",
                     event.hid.modifiers, event.hid.keycode);
            break;
        case BSP_KEYBOARD_EVENT_RAW:
            snprintf(line, sizeof(line), "RAW #%u %s",
                     event.raw.index, event.raw.pressed ? "down" : "up");
            break;
        case BSP_KEYBOARD_EVENT_CHAR:
            snprintf(line, sizeof(line), "CHAR mod=0x%02X \"%s\"",
                     event.chr.modifiers, event.chr.str);
            break;
        case BSP_KEYBOARD_EVENT_CONNECTED:
            snprintf(line, sizeof(line), "connected");
            break;
        case BSP_KEYBOARD_EVENT_DISCONNECTED:
            snprintf(line, sizeof(line), "disconnected");
            break;
        default:
            snprintf(line, sizeof(line), "event %d", (int)event.type);
            break;
    }
    append(line);
    updateStatus();
}

void KeyboardTestScreen::append(const std::string &line) {
    ESP_LOGI(TAG, "%s", line.c_str());
    lines_.push_back(line);
    while (lines_.size() > kLogLines) lines_.pop_front();
    std::string text;
    for (const auto &l : lines_) {
        text += l;
        text += '\n';
    }
    lv_label_set_text(log_, text.c_str());
}

void KeyboardTestScreen::updateStatus() {
    const uint32_t caps = bsp_keyboard_get_caps();
    lv_label_set_text_fmt(status_, "%s / mode %s / caps%s%s%s",
                          bsp_keyboard_is_connected() ? "connected" : "disconnected",
                          mode_name(bsp_keyboard_get_mode()),
                          (caps & BSP_KEYBOARD_CAP_HID)  ? " HID"  : "",
                          (caps & BSP_KEYBOARD_CAP_RAW)  ? " RAW"  : "",
                          (caps & BSP_KEYBOARD_CAP_CHAR) ? " CHAR" : "");
}
