/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

namespace {

constexpr int32_t kWidth = 400;
constexpr int32_t kRowHeight = 72;
constexpr int32_t kPadding = 24;
constexpr int32_t kGap = 8;
constexpr int32_t kMargin = 16;
constexpr uint32_t kCheckColor = 0x2196f3;

bool style_initialized = false;
lv_style_t card_style, row_style, row_pressed_style;

void style_init() {
    if (style_initialized) return;

    lv_style_init(&card_style);
    lv_style_set_bg_color(&card_style, lv_color_white());
    lv_style_set_bg_opa(&card_style, LV_OPA_COVER);
    lv_style_set_radius(&card_style, 16);
    lv_style_set_clip_corner(&card_style, true);
    lv_style_set_shadow_width(&card_style, 32);
    lv_style_set_shadow_color(&card_style, lv_color_black());
    lv_style_set_shadow_opa(&card_style, LV_OPA_30);
    lv_style_set_pad_ver(&card_style, 8);
    lv_style_set_layout(&card_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&card_style, LV_FLEX_FLOW_COLUMN);

    lv_style_init(&row_style);
    lv_style_set_width(&row_style, LV_PCT(100));
    lv_style_set_height(&row_style, kRowHeight);
    lv_style_set_pad_hor(&row_style, kPadding);
    lv_style_set_pad_column(&row_style, kPadding / 2);
    lv_style_set_layout(&row_style, LV_LAYOUT_FLEX);
    lv_style_set_flex_flow(&row_style, LV_FLEX_FLOW_ROW);
    lv_style_set_flex_main_place(&row_style, LV_FLEX_ALIGN_START);
    lv_style_set_flex_cross_place(&row_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_flex_track_place(&row_style, LV_FLEX_ALIGN_CENTER);
    lv_style_set_text_font(&row_style, lv_widgets_resolved_font(LV_WIDGETS_FONT_BODY));

    lv_style_init(&row_pressed_style);
    lv_style_set_bg_color(&row_pressed_style, lv_color_black());
    lv_style_set_bg_opa(&row_pressed_style, 35);

    style_initialized = true;
}

struct Popover {
    lv_obj_t *anchor = nullptr;
    lv_event_dsc_t *anchor_deleted = nullptr;
    bool closing = false;
};

Popover *popover_of(lv_obj_t *scrim) {
    return static_cast<Popover *>(lv_obj_get_user_data(scrim));
}

void delete_scrim(void *scrim) {
    lv_obj_delete(static_cast<lv_obj_t *>(scrim));
}

// Deferred: a row closes the popover from inside its own click event.
void close_scrim(lv_obj_t *scrim) {
    Popover *popover = popover_of(scrim);
    if (popover->closing) return;
    popover->closing = true;
    lv_async_call(delete_scrim, scrim);
}

void anchor_deleted(lv_event_t *e) {
    auto scrim = static_cast<lv_obj_t *>(lv_event_get_user_data(e));
    popover_of(scrim)->anchor = nullptr;
    close_scrim(scrim);
}

void scrim_deleted(lv_event_t *e) {
    auto scrim = static_cast<lv_obj_t *>(lv_event_get_current_target(e));
    Popover *popover = popover_of(scrim);
    lv_async_call_cancel(delete_scrim, scrim);
    if (popover->anchor) lv_obj_remove_event_dsc(popover->anchor, popover->anchor_deleted);
    delete popover;
}

void place(lv_obj_t *card) {
    lv_obj_t *scrim = lv_obj_get_parent(card);
    lv_obj_t *anchor = popover_of(scrim)->anchor;
    if (!anchor) return;

    lv_area_t target, frame;
    lv_obj_get_coords(anchor, &target);
    lv_obj_get_coords(scrim, &frame);
    const int32_t width = lv_obj_get_width(card);
    const int32_t height = lv_obj_get_height(card);
    const int32_t frame_width = lv_area_get_width(&frame);
    const int32_t frame_height = lv_area_get_height(&frame);

    int32_t x = target.x2 + 1 - width - frame.x1;
    int32_t y = target.y2 + 1 + kGap - frame.y1;
    if (y + height > frame_height - kMargin) y = target.y1 - kGap - height - frame.y1;
    x = LV_CLAMP(kMargin, x, frame_width - kMargin - width);
    y = LV_MAX(y, kMargin);
    lv_obj_set_pos(card, x, y);
}

}

lv_obj_t *lv_popover_open(lv_obj_t *anchor) {
    style_init();
    lv_obj_t *parent = lv_obj_get_screen(anchor);

    auto scrim = lv_container_create(parent);
    lv_obj_set_ignore_layout(scrim, true);
    lv_obj_update_layout(parent);
    lv_obj_set_pos(scrim, -lv_obj_get_style_space_left(parent, LV_PART_MAIN),
                   -lv_obj_get_style_space_top(parent, LV_PART_MAIN));
    lv_obj_set_size(scrim, lv_obj_get_width(parent), lv_obj_get_height(parent));
    lv_obj_set_clickable(scrim, true);
    lv_obj_set_scrollable(scrim, false);

    auto popover = new Popover;
    popover->anchor = anchor;
    popover->anchor_deleted = lv_obj_add_event_cb(anchor, anchor_deleted, LV_EVENT_DELETE, scrim);
    lv_obj_set_user_data(scrim, popover);
    lv_obj_add_event_cb(scrim, scrim_deleted, LV_EVENT_DELETE, nullptr);
    lv_obj_add_event_fn(scrim, LV_EVENT_CLICKED, [scrim](lv_event_t *e) {
        if (lv_event_get_target(e) == scrim) close_scrim(scrim);
    });

    auto card = lv_container_create(scrim);
    lv_obj_add_style(card, &card_style, 0);
    lv_obj_set_size(card, kWidth, LV_SIZE_CONTENT);
    lv_obj_set_clickable(card, true);
    lv_obj_set_scrollable(card, false);
    lv_obj_add_event_fn(card, LV_EVENT_SIZE_CHANGED, [card](lv_event_t *) { place(card); });
    return card;
}

void lv_popover_close(lv_obj_t *popover) {
    close_scrim(lv_obj_get_parent(popover));
}

lv_obj_t *lv_popover_heading_create(lv_obj_t *popover, const char *text) {
    auto label = lv_label_create(popover);
    lv_label_set_text(label, text);
    lv_obj_set_width(label, LV_PCT(100));
    lv_obj_set_style_pad_hor(label, kPadding, 0);
    lv_obj_set_style_pad_ver(label, 8, 0);
    lv_obj_set_font_role(label, LV_WIDGETS_FONT_BODY);
    lv_obj_set_style_text_color(label, lv_color_hex(0x808080), 0);
    return label;
}

lv_obj_t *lv_popover_separator_create(lv_obj_t *popover) {
    auto separator = lv_hor_separator_create(popover);
    lv_obj_set_style_margin_ver(separator, 8, 0);
    return separator;
}

lv_obj_t *lv_popover_row_create(lv_obj_t *popover, const char *icon, const lv_font_t *icon_font,
                                const char *text, std::function<void(lv_event_t *)> on_click) {
    style_init();
    auto row = lv_button_create(popover);
    lv_obj_remove_style_all(row);
    lv_obj_add_style(row, &row_style, 0);
    lv_obj_add_style(row, &row_pressed_style, LV_STATE_PRESSED);

    auto icon_label = lv_label_create(row);
    lv_obj_set_width(icon_label, 48);
    lv_obj_set_style_text_align(icon_label, LV_TEXT_ALIGN_CENTER, 0);
    if (icon_font) lv_obj_set_style_text_font(icon_label, icon_font, 0);
    lv_label_set_text(icon_label, icon ? icon : "");
    lv_obj_set_hidden(icon_label, !icon);

    auto label = lv_label_create(row);
    lv_obj_set_flex_grow(label, 1);
    lv_label_set_long_mode(label, LV_LABEL_LONG_MODE_DOTS);
    lv_label_set_text(label, text);

    auto check = lv_label_create(row);
    lv_obj_set_width(check, 48);
    lv_obj_set_style_text_align(check, LV_TEXT_ALIGN_CENTER, 0);
    lv_obj_set_style_text_color(check, lv_color_hex(kCheckColor), 0);
    lv_label_set_text(check, LV_SYMBOL_OK);
    lv_obj_set_hidden(check, true);

    lv_obj_add_event_fn(row, LV_EVENT_CLICKED, [popover, on_click](lv_event_t *e) {
        if (on_click) on_click(e);
        lv_popover_close(popover);
    });
    return row;
}

void lv_popover_row_set_checked(lv_obj_t *row, bool checked) {
    lv_obj_set_hidden(lv_obj_get_child(row, 2), !checked);
}
