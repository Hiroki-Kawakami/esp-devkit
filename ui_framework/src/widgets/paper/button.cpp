/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

namespace {

bool style_initialized = false;
lv_style_t default_button_style;
lv_style_t default_button_pressed_style;
lv_style_t plain_button_style;

void button_style_init() {
    if (style_initialized) return;

    lv_style_init(&default_button_style);
    lv_style_init(&default_button_pressed_style);
    lv_style_set_pad_all(&default_button_style, 8);
    lv_style_set_text_font(&default_button_style, &lv_font_montserrat_20);
    lv_style_set_border_width(&default_button_style, 1);
    lv_style_set_border_opa(&default_button_style, LV_OPA_COVER);
    lv_style_set_border_color(&default_button_style, lv_color_white());
    lv_style_set_border_color(&default_button_pressed_style, lv_color_black());
    lv_style_set_outline_width(&default_button_style, 1);
    lv_style_set_outline_opa(&default_button_style, LV_OPA_COVER);
    lv_style_set_outline_color(&default_button_style, lv_color_black());

    lv_style_init(&plain_button_style);
    lv_style_set_pad_all(&plain_button_style, 8);
    lv_style_set_text_font(&plain_button_style, &lv_font_montserrat_20);
    lv_style_set_border_width(&plain_button_style, 2);
    lv_style_set_border_opa(&plain_button_style, LV_OPA_COVER);
    lv_style_set_radius(&plain_button_style, 8);
    lv_style_set_border_color(&plain_button_style, lv_color_white());

    style_initialized = true;
}

}

lv_obj_t *lv_button_create(lv_obj_t *parent, lv_button_style_t type) {
    button_style_init();
    lv_obj_t *button = lv_button_create(parent);
    lv_obj_remove_style_all(button);
    if (type == LV_BUTTON_STYLE_PLAIN) {
        lv_obj_add_style(button, &plain_button_style, 0);
        lv_obj_add_style(button, &default_button_pressed_style, LV_STATE_PRESSED);
    } else {
        lv_obj_add_style(button, &default_button_style, 0);
        lv_obj_add_style(button, &default_button_pressed_style, LV_STATE_PRESSED);
    }
    return button;
}

lv_obj_t *lv_button_set_text(lv_obj_t *button, const char *text, const lv_font_t *font) {
    lv_obj_t *label = lv_label_create(button);
    lv_obj_center(label);
    lv_label_set_text(label, text);
    if (font != nullptr) {
        lv_obj_set_style_text_font(label, font, 0);
    }
    return label;
}
