/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets/button.hpp"
#include "widgets.hpp"
#include <src/misc/lv_color.h>
#include <src/misc/lv_style.h>

namespace {

const int kButtonColorPrimary = 0x2196f3;
const int kButtonColorSecondary = 0xe0e0e0;
const int kButtonColorDestructive = 0xd32f2f;

bool style_initialized = false;
lv_style_t base_style, pressed_style;
lv_style_t primary_style, primary_outline_style;
lv_style_t secondary_style, secondary_outline_style;
lv_style_t destructive_style, destructive_outline_style;
lv_style_t plain_outline_style, plain_pressed_style;

void button_style_init() {
    if (style_initialized) return;

    lv_style_init(&base_style);
    lv_style_set_text_font(&base_style, &lv_font_montserrat_20);
    lv_style_set_radius(&base_style, 12);
    lv_style_set_pad_all(&base_style, 8);

    lv_style_init(&pressed_style);
    lv_style_set_recolor(&pressed_style, lv_color_black());
    lv_style_set_recolor_opa(&pressed_style, 35);
    lv_style_set_transform_width(&pressed_style, 1);
    lv_style_set_transform_height(&pressed_style, 1);

    auto create_style = [](lv_style_t *style, lv_style_t *outline_style, lv_color_t color, lv_color_t text_color) {
        if (style) {
            lv_style_init(style);
            lv_style_set_bg_color(style, color);
            lv_style_set_bg_opa(style, LV_OPA_COVER);
            lv_style_set_text_color(style, text_color);
        }
        if (outline_style) {
            lv_style_init(outline_style);
            lv_style_set_bg_color(outline_style, lv_color_white());
            lv_style_set_bg_opa(outline_style, LV_OPA_COVER);
            lv_style_set_border_width(outline_style, 1);
            lv_style_set_border_color(outline_style, color);
            lv_style_set_border_opa(outline_style, LV_OPA_COVER);
            lv_style_set_text_color(outline_style, color);
        }
    };
    create_style(&primary_style, &primary_outline_style, lv_color_hex(kButtonColorPrimary), lv_color_white());
    create_style(&secondary_style, nullptr, lv_color_hex(kButtonColorSecondary), lv_color_black());
    create_style(nullptr, &secondary_outline_style, lv_color_black(), lv_color_black());
    create_style(&destructive_style, &destructive_outline_style, lv_color_hex(kButtonColorDestructive), lv_color_white());

    lv_style_init(&plain_outline_style);
    lv_style_set_border_width(&plain_outline_style, 1);
    lv_style_set_border_color(&plain_outline_style, lv_color_black());
    lv_style_set_border_opa(&plain_outline_style, LV_OPA_COVER);

    lv_style_init(&plain_pressed_style);
    lv_style_set_bg_color(&plain_pressed_style, lv_color_black());
    lv_style_set_bg_opa(&plain_pressed_style, 35);

    style_initialized = true;
}

}

lv_obj_t *lv_button_create(lv_obj_t *parent, lv_button_style_t type) {
    button_style_init();

    lv_obj_t *button = lv_button_create(parent);
    lv_obj_remove_style_all(button);
    lv_obj_add_style(button, &base_style, 0);

    switch (type) {
    case LV_BUTTON_STYLE_DEFAULT:
    case LV_BUTTON_STYLE_PRIMARY:
        lv_obj_add_style(button, &primary_style, 0);
        lv_obj_add_style(button, &pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_SECONDARY:
        lv_obj_add_style(button, &secondary_style, 0);
        lv_obj_add_style(button, &pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_DESTRUCTIVE:
        lv_obj_add_style(button, &destructive_style, 0);
        lv_obj_add_style(button, &pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_PRIMARY | LV_BUTTON_STYLE_OUTLINE:
        lv_obj_add_style(button, &primary_outline_style, 0);
        lv_obj_add_style(button, &pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_DEFAULT | LV_BUTTON_STYLE_OUTLINE:
    case LV_BUTTON_STYLE_SECONDARY | LV_BUTTON_STYLE_OUTLINE:
        lv_obj_add_style(button, &secondary_outline_style, 0);
        lv_obj_add_style(button, &pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_DESTRUCTIVE | LV_BUTTON_STYLE_OUTLINE:
        lv_obj_add_style(button, &destructive_outline_style, 0);
        lv_obj_add_style(button, &pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_PLAIN:
        lv_obj_add_style(button, &plain_pressed_style, LV_STATE_PRESSED);
        break;
    case LV_BUTTON_STYLE_PLAIN | LV_BUTTON_STYLE_OUTLINE:
        lv_obj_add_style(button, &plain_outline_style, 0);
        lv_obj_add_style(button, &plain_pressed_style, LV_STATE_PRESSED);
        break;
    default:
        break;
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
