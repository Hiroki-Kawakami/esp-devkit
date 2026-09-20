/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "home_screen.hpp"
#include "format_test_screen.hpp"
#include "render_format_screen.hpp"

static constexpr int kCycleSwitches = 40;
static constexpr uint32_t kCyclePeriodMs = 1500;

static lv_obj_t *add_button(lv_obj_t *parent, const char *text) {
    auto button = lv_button_create(parent);
    lv_obj_set_width(button, LV_PCT(100));
    auto title = lv_label_create(button);
    lv_obj_center(title);
    lv_label_set_text(title, text);
    return button;
}

void HomeScreen::build() {
    createNavigation("HomeScreen");

    auto label = lv_label_create(contents_);
    lv_label_set_text(label, "Hello, Tab5!");
    lv_obj_set_style_text_font(label, &lv_font_montserrat_48, 0);
    lv_obj_set_style_text_color(label, lv_color_black(), 0);

    auto button = add_button(contents_, "Open Modal");
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, [this](lv_event_t*){
        auto modal = lv_modal_open(root_);
        lv_modal_title_create(modal, "Modal Title");
        lv_modal_message_create(modal, "Modal Message\n12345");
        lv_modal_button_create(modal, "Close", LV_MODAL_BUTTON_TYPE_PRIMARY, [modal](lv_event_t*){
            lv_modal_close(modal);
        });
    });

    auto format_test = add_button(contents_, "Pixel Format Test");
    lv_obj_add_event_fn(format_test, LV_EVENT_CLICKED, [](lv_event_t*){
        screen_manager.push(std::make_shared<FormatTestScreen>());
    });

    auto render_format = add_button(contents_, "Render Format Test");
    lv_obj_add_event_fn(render_format, LV_EVENT_CLICKED, [](lv_event_t*){
        screen_manager.push(std::make_shared<RenderFormatScreen>());
    });

    auto format_cycle = add_button(contents_, "Pixel Format Cycle");
    lv_obj_add_event_fn(format_cycle, LV_EVENT_CLICKED, [](lv_event_t*){
        static int tick;
        tick = 0;
        auto timer = lv_timer_create([](lv_timer_t *) {
            if (tick++ % 2) {
                screen_manager.pop();
            } else {
                screen_manager.push(std::make_shared<FormatTestScreen>());
            }
        }, kCyclePeriodMs, nullptr);
        lv_timer_set_repeat_count(timer, kCycleSwitches);
    });
}
