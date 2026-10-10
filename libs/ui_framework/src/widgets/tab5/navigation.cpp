/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"
#include "screen_manager.hpp"

namespace {

const int kNavigationBorderColor = 0xc0c0c0;

bool style_initialized = false;
lv_style_t base_style, default_style;
lv_style_t back_button_style;
lv_style_t title_area_style, actions_style;

void navigation_style_init() {
    if (style_initialized) return;

    lv_style_init(&base_style);
    lv_style_set_size(&base_style, LV_PCT(100), 120);
    lv_style_set_layout(&base_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&base_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_main_place(&base_style, LV_FLEX_ALIGN_START);
    lv_style_set_flex_cross_place(&base_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&base_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_pad_hor(&base_style, 24);
    lv_style_set_pad_column(&base_style, 24);

    lv_style_init(&default_style);
    lv_style_set_bg_color(&default_style, lv_color_white());
    lv_style_set_bg_opa(&default_style, LV_OPA_COVER);
    lv_style_set_border_color(&default_style, lv_color_hex(kNavigationBorderColor));
    lv_style_set_border_width(&default_style, 1);
    lv_style_set_border_side(&default_style, LV_BORDER_SIDE_BOTTOM);

    lv_style_init(&back_button_style);
    lv_style_set_layout(&back_button_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&back_button_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_main_place(&back_button_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_cross_place(&back_button_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&back_button_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_pad_all(&back_button_style, 8);
    lv_style_set_pad_column(&back_button_style, 16);

    lv_style_init(&title_area_style);
    lv_style_set_height(&title_area_style, LV_SIZE_CONTENT);
    lv_style_set_flex_grow(&title_area_style, 1);
    lv_style_set_layout(&title_area_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&title_area_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_cross_place(&title_area_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&title_area_style, LV_FLEX_ALIGN_CENTER);

    lv_style_init(&actions_style);
    lv_style_set_size(&actions_style, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    lv_style_set_layout(&actions_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&actions_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_cross_place(&actions_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&actions_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_pad_column(&actions_style, 8);

    style_initialized = true;
}

void single_line(lv_obj_t *label) {
    lv_label_set_long_mode(label, LV_LABEL_LONG_MODE_DOTS);
    lv_obj_set_height(label, lv_font_get_line_height(lv_obj_get_style_text_font(label, LV_PART_MAIN)));
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
    lv_obj_t *area = lv_container_create(parent);
    lv_obj_add_style(area, &title_area_style, 0);
    lv_obj_move_to_index(area, 0);

    auto button = lv_button_create(area, LV_BUTTON_STYLE_NAVIGATION);
    lv_obj_add_style(button, &back_button_style, 0);
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, back);

    lv_obj_t *icon = lv_label_create(button);
    lv_label_set_text(icon, LV_SYMBOL_LEFT);
    lv_obj_set_font_role(icon, LV_WIDGETS_FONT_ICON);
    lv_obj_set_style_pad_all(icon, 8, 0);
    lv_obj_t *label = lv_label_create(button);
    lv_label_set_text(label, title);
    lv_obj_set_font_role(label, LV_WIDGETS_FONT_TITLE);
    single_line(label);
    lv_obj_set_user_data(button, label);

    lv_obj_add_event_fn(area, LV_EVENT_SIZE_CHANGED, [area, button, icon, label](lv_event_t *) {
        lv_obj_refr_size(icon);
        const int32_t chrome = lv_obj_get_style_pad_left(button, LV_PART_MAIN) + lv_obj_get_style_pad_right(button, LV_PART_MAIN) +
                               lv_obj_get_style_pad_column(button, LV_PART_MAIN) + lv_obj_get_width(icon);
        lv_obj_set_style_max_width(label, LV_MAX(lv_obj_get_content_width(area) - chrome, 0), 0);
    });

    return button;
}

lv_obj_t *lv_navigation_back_label(lv_obj_t *back) {
    return static_cast<lv_obj_t *>(lv_obj_get_user_data(back));
}

lv_obj_t *lv_navigation_title_create(lv_obj_t *parent, const char *title) {
    lv_obj_t *label = lv_label_create(parent);
    lv_label_set_text(label, title);
    lv_obj_set_font_role(label, LV_WIDGETS_FONT_TITLE);
    lv_obj_set_style_pad_hor(label, 8, 0);
    lv_obj_set_flex_grow(label, 1);
    single_line(label);
    lv_obj_move_to_index(label, 0);
    return label;
}

lv_obj_t *lv_navigation_actions(lv_obj_t *navigation) {
    auto actions = static_cast<lv_obj_t *>(lv_obj_get_user_data(navigation));
    if (actions) return actions;
    actions = lv_container_create(navigation);
    lv_obj_add_style(actions, &actions_style, 0);
    lv_obj_set_user_data(navigation, actions);
    return actions;
}

void NavigationScreen::back() {
    screen_manager.pop();
}

void NavigationScreen::createNavigation(const char *title, lv_navigation_style_t style) {
    lv_navigation_style_t layout_style = style & ~LV_NAVIGATION_STYLE_BACK;
    bool back = style & LV_NAVIGATION_STYLE_BACK;

    lv_obj_set_flex_flow(root_, LV_FLEX_FLOW_COLUMN);
    if (layout_style == LV_NAVIGATION_STYLE_LIST) {
        lv_obj_set_style_bg_color(root_, lv_color_white(), 0);
    } else {
        lv_obj_set_style_bg_color(root_, lv_color_hex(0xeeeeee), 0);
    }
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
        lv_obj_set_style_pad_all(contents_, 24, 0);
        lv_obj_set_style_pad_row(contents_, 24, 0);
        break;
    case LV_NAVIGATION_STYLE_UNIFIED:
        lv_obj_set_style_pad_hor(contents_, 24, 0);
        lv_obj_set_style_pad_bottom(contents_, 24, 0);
        lv_obj_set_style_pad_row(contents_, 24, 0);
        break;
    default:
        lv_obj_set_style_pad_all(contents_, 0, 0);
        lv_obj_set_style_pad_row(contents_, 0, 0);
        break;
    }
}
