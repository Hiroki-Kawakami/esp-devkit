/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"
#include "screen_manager.hpp"

lv_obj_t *lv_navigation_create(lv_obj_t *parent) {
    auto navigation = lv_container_create(parent, lv_color_white());
    lv_obj_set_size(navigation, LV_PCT(100), 120);
    lv_obj_remove_flag(navigation, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_style_border_side(navigation, LV_BORDER_SIDE_BOTTOM, 0);
    lv_obj_set_style_border_width(navigation, 1, 0);
    lv_obj_set_style_border_color(navigation, lv_color_hex(0xc0c0c0), 0);
    lv_obj_set_flex_flow(navigation, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(navigation, LV_FLEX_ALIGN_START, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_hor(navigation, 24, 0);
    return navigation;
}

lv_obj_t *lv_navigation_back_create(lv_obj_t *parent, const char *title,
                                    std::function<void(lv_event_t *)> back) {
    lv_obj_t *button = lv_button_create(parent);
    lv_obj_remove_style_all(button);
    lv_obj_set_flex_flow(button, LV_FLEX_FLOW_ROW);
    lv_obj_set_flex_align(button, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    lv_obj_set_style_pad_all(button, 8, 0);
    lv_obj_set_style_pad_column(button, 16, 0);
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, back);
    lv_obj_set_style_bg_color(button, lv_color_hex(0xe0e0e0), LV_STATE_PRESSED);
    lv_obj_set_style_bg_opa(button, LV_OPA_COVER, LV_STATE_PRESSED);
    lv_obj_set_style_radius(button, 12, 0);

    lv_obj_t *icon = lv_label_create(button);
    lv_label_set_text(icon, LV_SYMBOL_LEFT);
    lv_obj_set_style_text_font(icon, &lv_font_montserrat_38, 0);
    lv_obj_set_style_pad_all(icon, 8, 0);
    lv_obj_t *label = lv_label_create(button);
    lv_label_set_text(label, title);
    lv_obj_set_style_text_font(label, &lv_font_montserrat_38, 0);
    lv_obj_set_user_data(button, label);

    return button;
}

lv_obj_t *lv_navigation_back_label(lv_obj_t *back) {
    return static_cast<lv_obj_t *>(lv_obj_get_user_data(back));
}

void NavigationScreen::back() {
    screen_manager.pop();
}

void NavigationScreen::createNavigation(const char *title) {
    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_bg_color(root_, lv_color_hex(0xeeeeee), 0);
    lv_obj_set_style_pad_row(root_, 0, 0);

    navigation_ = lv_navigation_create(root_);
    auto button = lv_navigation_back_create(navigation_, title,
        [this](lv_event_t *) { back(); });
    navigation_title_ = lv_navigation_back_label(button);

    contents_ = lv_spacer_create(root_, LV_PCT(100), LV_SIZE_CONTENT, 1);
    lv_obj_set_flex_flow(contents_, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(contents_, 24, 0);
    lv_obj_set_style_pad_all(contents_, 24, 0);
}
