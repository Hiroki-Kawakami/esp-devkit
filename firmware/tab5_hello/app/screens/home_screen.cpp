/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "home_screen.hpp"

void HomeScreen::build() {
    createNavigation("HomeScreen");

    auto label = lv_label_create(contents_);
    lv_label_set_text(label, "Hello, Tab5!");
    lv_obj_set_style_text_font(label, &lv_font_montserrat_48, 0);
    lv_obj_set_style_text_color(label, lv_color_black(), 0);

    auto button = lv_button_create(contents_);
    lv_obj_set_width(button, LV_PCT(100));
    auto title = lv_label_create(button);
    lv_obj_center(title);
    lv_label_set_text(title, "Open Modal");
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, [this](lv_event_t*){
        auto modal = lv_modal_open(root_);
        lv_modal_title_create(modal, "Modal Title");
        lv_modal_message_create(modal, "Modal Message\n12345");
        lv_modal_button_create(modal, "Close", LV_MODAL_BUTTON_TYPE_PRIMARY, [modal](lv_event_t*){
            lv_modal_close(modal);
        });
    });

}
