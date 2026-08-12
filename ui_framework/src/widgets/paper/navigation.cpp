/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"
#include "screen_manager.hpp"

namespace {

bool style_initialized = false;
lv_style_t base_style, default_style;
lv_style_t back_button_style;

void navigation_style_init() {
    if (style_initialized) return;

    lv_style_init(&base_style);
    lv_style_set_size(&base_style, LV_PCT(100), 100);
    lv_style_set_layout(&base_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&base_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_main_place(&base_style, LV_FLEX_ALIGN_START);
    lv_style_set_flex_cross_place(&base_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&base_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_pad_hor(&base_style, 12);
    lv_style_set_pad_column(&base_style, 12);

    lv_style_init(&default_style);
    lv_style_set_bg_color(&default_style, lv_color_white());
    lv_style_set_bg_opa(&default_style, LV_OPA_COVER);
    lv_style_set_border_color(&default_style, lv_color_black());
    lv_style_set_border_width(&default_style, 2);
    lv_style_set_border_side(&default_style, LV_BORDER_SIDE_BOTTOM);

    lv_style_init(&back_button_style);
    lv_style_set_layout(&back_button_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&back_button_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_main_place(&back_button_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_cross_place(&back_button_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&back_button_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_pad_all(&back_button_style, 8);
    lv_style_set_pad_column(&back_button_style, 16);

    style_initialized = true;
}

}

lv_obj_t *lv_navigation_create(lv_obj_t *parent, lv_navigation_style_t style) {
    navigation_style_init();
    auto navigation = lv_container_create(parent);
    lv_obj_add_style(navigation, &base_style, 0);
    if (style != LV_NAVIGATION_STYLE_UNIFIED) {
        lv_obj_add_style(navigation, &default_style, 0);
    }
    return navigation;
}

lv_obj_t *lv_navigation_back_create(lv_obj_t *parent, const char *title, std::function<void(lv_event_t *)> back) {
    auto button = lv_button_create(parent, LV_BUTTON_STYLE_PLAIN);
    lv_obj_add_style(button, &back_button_style, 0);
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, back);

    auto icon = lv_label_create(button);
    lv_label_set_text(icon, LV_SYMBOL_LEFT);
    lv_obj_set_style_text_font(icon, lv_widgets_title_font(), 0);
    lv_obj_set_style_pad_all(icon, 8, 0);
    auto label = lv_label_create(button);
    lv_label_set_text(label, title);
    lv_obj_set_style_text_font(label, lv_widgets_title_font(), 0);
    lv_obj_set_user_data(button, label);

    return button;
}

lv_obj_t *lv_navigation_back_label(lv_obj_t *back) {
    return static_cast<lv_obj_t *>(lv_obj_get_user_data(back));
}

lv_obj_t *lv_navigation_title_create(lv_obj_t *parent, const char *title) {
    lv_obj_t *label = lv_label_create(parent);
    lv_label_set_text(label, title);
    lv_obj_set_style_text_font(label, lv_widgets_title_font(), 0);
    lv_obj_set_style_pad_hor(label, 16, 0);
    return label;
}

void NavigationScreen::back() {
    screen_manager.pop();
}

void NavigationScreen::createNavigation(const char *title, lv_navigation_style_t style) {
    lv_navigation_style_t layout_style = style & ~LV_NAVIGATION_STYLE_BACK;
    bool back = style & LV_NAVIGATION_STYLE_BACK;

    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_bg_color(root_, lv_color_white(), 0);
    lv_obj_set_style_pad_row(root_, 0, 0);

    navigation_ = lv_navigation_create(root_, layout_style);
    if (back) {
        auto button = lv_navigation_back_create(navigation_, title,
            [this](lv_event_t *) { this->back(); });
        navigation_title_ = lv_navigation_back_label(button);
    } else {
        navigation_title_ = lv_navigation_title_create(navigation_, title);
    }

    contents_ = lv_spacer_create(root_, LV_PCT(100), LV_SIZE_CONTENT, 1);
    lv_obj_set_flex_flow(contents_, LV_FLEX_FLOW_COLUMN);
    switch (layout_style) {
    case LV_NAVIGATION_STYLE_DEFAULT:
        lv_obj_set_style_pad_all(contents_, 20, 0);
        lv_obj_set_style_pad_row(contents_, 20, 0);
        break;
    case LV_NAVIGATION_STYLE_UNIFIED:
        lv_obj_set_style_pad_hor(contents_, 20, 0);
        lv_obj_set_style_pad_bottom(contents_, 20, 0);
        lv_obj_set_style_pad_row(contents_, 20, 0);
        break;
    default:
        lv_obj_set_style_pad_all(contents_, 0, 0);
        lv_obj_set_style_pad_row(contents_, 0, 0);
        break;
    }
}
