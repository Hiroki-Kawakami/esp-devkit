/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "home_screen.hpp"
#include "button_widgets_screen.hpp"

void HomeScreen::build() {
    createNavigation("UI Test", false);

    create_row("Button Widgets", [](lv_event_t*){
        screen_manager.push(std::make_shared<ButtonWidgetsScreen>());
    });
}

lv_obj_t *HomeScreen::create_row(const char *title, std::function<void(lv_event_t*)> on_click) {
    auto button = lv_button_create(contents_);
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, on_click);
    auto label = lv_label_create(button);
    lv_label_set_text(label, title);
    return button;
}
