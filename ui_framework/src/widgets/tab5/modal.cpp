/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

namespace {
constexpr uint32_t COLOR_SURFACE = 0xFFFFFF;
constexpr uint32_t COLOR_TEXT = 0x202124;
constexpr uint32_t COLOR_PRIMARY = 0x2563EB;
constexpr uint32_t COLOR_SECONDARY = 0xE8ECF2;
constexpr uint32_t COLOR_DESTRUCTIVE = 0xDC2626;
}  // namespace

lv_obj_t *lv_modal_open(lv_obj_t *parent) {
    auto scrim = lv_obj_create(parent);
    lv_obj_remove_style_all(scrim);
    lv_obj_add_flag(scrim, LV_OBJ_FLAG_IGNORE_LAYOUT);
    lv_obj_update_layout(parent);
    lv_obj_set_pos(scrim, -lv_obj_get_style_space_left(parent, LV_PART_MAIN),
                  -lv_obj_get_style_space_top(parent, LV_PART_MAIN));
    lv_obj_set_size(scrim, lv_obj_get_width(parent), lv_obj_get_height(parent));
    lv_obj_set_style_bg_color(scrim, lv_color_black(), 0);
    lv_obj_set_style_bg_opa(scrim, LV_OPA_50, 0);
    lv_obj_add_flag(scrim, LV_OBJ_FLAG_CLICKABLE);  // absorb taps behind the card
    lv_obj_remove_flag(scrim, LV_OBJ_FLAG_SCROLLABLE);

    auto card = lv_obj_create(scrim);
    lv_obj_set_width(card, 600);
    lv_obj_set_height(card, LV_SIZE_CONTENT);
    lv_obj_center(card);
    lv_obj_set_style_radius(card, 16, 0);
    lv_obj_set_style_pad_all(card, 32, 0);
    lv_obj_set_flex_flow(card, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(card, 24, 0);
    lv_obj_remove_flag(card, LV_OBJ_FLAG_SCROLLABLE);
    return card;
}

void lv_modal_close(lv_obj_t *card) {
    lv_obj_delete(lv_obj_get_parent(card));
}

lv_obj_t *lv_modal_title_create(lv_obj_t *parent, const char *text) {
    auto label = lv_label_create(parent);
    lv_label_set_text(label, text);
    lv_obj_set_style_text_font(label, &lv_font_montserrat_28, 0);
    lv_obj_set_style_text_align(label, LV_TEXT_ALIGN_LEFT, 0);

    if (lv_obj_get_style_flex_flow(parent, LV_PART_MAIN) == LV_FLEX_FLOW_ROW) {
        lv_obj_set_flex_grow(label, 1);
    } else {
        lv_obj_set_width(label, LV_PCT(100));
    }
    return label;
}

lv_obj_t *lv_modal_message_create(lv_obj_t *parent, const char *text) {
    auto label = lv_label_create(parent);
    lv_label_set_text(label, text);
    lv_obj_set_style_text_font(label, &lv_font_montserrat_20, 0);
    lv_obj_set_style_text_color(label, lv_color_hex(0x444444), 0);
    lv_obj_set_style_text_align(label, LV_TEXT_ALIGN_LEFT, 0);

    if (lv_obj_get_style_flex_flow(parent, LV_PART_MAIN) == LV_FLEX_FLOW_ROW) {
        lv_obj_set_flex_grow(label, 1);
    } else {
        lv_obj_set_width(label, LV_PCT(100));
    }
    return label;
}

lv_obj_t *lv_modal_button_create(lv_obj_t *parent, const char *text,
                                 lv_modal_button_type type,
                                 std::function<void(lv_event_t *)> on_click) {
    lv_color_t background = lv_color_hex(0x2196F3);
    lv_color_t foreground = lv_color_white();
    if (type == LV_MODAL_BUTTON_TYPE_SECONDARY) {
        background = lv_color_hex(0xe0e0e0);
        foreground = lv_color_black();
    } else if (type == LV_MODAL_BUTTON_TYPE_DESTRUCTIVE) {
        background = lv_color_hex(0xd32f2f);
    }

    auto button = lv_button_create(parent);
    lv_obj_set_height(button, 72);
    lv_obj_set_style_radius(button, 12, 0);
    lv_obj_set_style_bg_color(button, background, 0);
    lv_obj_set_style_shadow_width(button, 0, 0);
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, on_click);

    if (lv_obj_get_style_flex_flow(parent, LV_PART_MAIN) == LV_FLEX_FLOW_ROW) {
        lv_obj_set_flex_grow(button, 1);
    } else {
        lv_obj_set_width(button, LV_PCT(100));
    }

    auto label = lv_label_create(button);
    lv_obj_center(label);
    lv_label_set_text(label, text);
    lv_obj_set_style_text_font(label, lv_widgets_body_font(), 0);
    lv_obj_set_style_text_color(label, foreground, 0);
    return button;
}
