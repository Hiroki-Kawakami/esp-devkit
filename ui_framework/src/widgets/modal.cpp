/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

lv_obj_t *lv_modal_open(lv_obj_t *parent) {
    auto scrim = lv_container_create(parent);
    lv_obj_add_flag(scrim, LV_OBJ_FLAG_FLOATING);
    lv_obj_update_layout(parent);
    lv_obj_set_pos(scrim, -lv_obj_get_style_space_left(parent, LV_PART_MAIN),
                  -lv_obj_get_style_space_top(parent, LV_PART_MAIN));
    lv_obj_set_size(scrim, lv_obj_get_width(parent), lv_obj_get_height(parent));
    lv_obj_add_flag(scrim, LV_OBJ_FLAG_CLICKABLE);  // absorb taps behind the card
    lv_obj_remove_flag(scrim, LV_OBJ_FLAG_SCROLLABLE);

    auto card = lv_container_create(scrim, lv_color_white());
    lv_obj_set_size(card, 480, LV_SIZE_CONTENT);
    lv_obj_center(card);
    lv_obj_set_style_border_width(card, 1, 0);
    lv_obj_set_style_border_color(card, lv_color_black(), 0);
    lv_obj_set_style_pad_all(card, 20, 0);
    lv_obj_set_flex_flow(card, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(card, 20, 0);
    lv_obj_remove_flag(card, LV_OBJ_FLAG_SCROLLABLE);
    return card;
}

void lv_modal_close(lv_obj_t *card) {
    lv_obj_delete(lv_obj_get_parent(card));  // the scrim owns the card
}

lv_obj_t *lv_modal_title_create(lv_obj_t *parent, const char *text) {
    auto label = lv_label_create(parent);
    lv_label_set_text(label, text);
    lv_obj_set_style_text_font(label, lv_widgets_title_font(), 0);
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
    lv_obj_set_style_text_align(label, LV_TEXT_ALIGN_LEFT, 0);

    if (lv_obj_get_style_flex_flow(parent, LV_PART_MAIN) == LV_FLEX_FLOW_ROW) {
        lv_obj_set_flex_grow(label, 1);
    } else {
        lv_obj_set_width(label, LV_PCT(100));
    }

    return label;
}


lv_obj_t *lv_modal_button_create(lv_obj_t *parent, const char *text, lv_modal_button_type type, std::function<void(lv_event_t*)> on_click) {
    auto button = lv_button_create(parent);
    lv_obj_set_height(button, 64);
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

    return button;
}
