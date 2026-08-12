/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "button_widgets_screen.hpp"

void ButtonWidgetsScreen::build() {
    createNavigation("Button Widgets");
    lv_obj_set_flex_align(contents_, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER);

    create_sample("Default", LV_BUTTON_TYPE_DEFAULT);
    create_sample("Primary", LV_BUTTON_TYPE_PRIMARY);
    create_sample("Secondary", LV_BUTTON_TYPE_SECONDARY);
    create_sample("Destructive", LV_BUTTON_TYPE_DESTRUCTIVE);
    create_sample("Plain", LV_BUTTON_TYPE_PLAIN);
}

void ButtonWidgetsScreen::create_sample(const char *title, lv_button_type_t type) {
    auto label = lv_label_create(contents_);
    lv_label_set_text(label, title);
    lv_obj_set_style_margin_all(label, 8, 0);

    auto row = lv_container_create(contents_, LV_FLEX_FLOW_ROW);
    lv_obj_set_size(row, LV_PCT(100), LV_SIZE_CONTENT);
    lv_obj_set_style_pad_all(row, 8, 0);
    lv_obj_set_style_pad_column(row, 8, 0);

    auto button1 = lv_button_create(row, type);
    lv_button_set_text(button1, "Button");

    auto button2 = lv_button_create(row, type | LV_BUTTON_TYPE_OUTLINE);
    lv_button_set_text(button2, "Button");

    lv_hor_separator_create(contents_);

}
