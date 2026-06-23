/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Layout widgets — panel-agnostic helpers over LVGL for building flex layouts:
 * style-stripped containers (optionally flex/colored), grow-able spacers, and
 * thin horizontal/vertical separators. Pulled in by lvgl.hpp.
 */

#pragma once
#include "lvgl.h"
#include <optional>

// MARK: Container
inline lv_obj_t *lv_container_create(lv_obj_t *parent) {
    auto obj = lv_obj_create(parent);
    lv_obj_remove_style_all(obj);
    return obj;
}
inline lv_obj_t *lv_container_create(lv_obj_t *parent, lv_flex_flow_t flow) {
    auto obj = lv_container_create(parent);
    lv_obj_set_flex_flow(obj, flow);
    if (flow == LV_FLEX_FLOW_ROW || flow == LV_FLEX_FLOW_COLUMN) {
        lv_obj_set_flex_align(obj, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER);
    }
    return obj;
}
inline lv_obj_t *lv_container_create(lv_obj_t *parent, lv_color_t bg_color) {
    auto obj = lv_container_create(parent);
    lv_obj_set_style_bg_color(obj, bg_color, 0);
    lv_obj_set_style_bg_opa(obj, LV_OPA_COVER, 0);
    return obj;
}

// MARK: Spacer
inline lv_obj_t *lv_spacer_create(lv_obj_t *parent, int32_t width, int32_t height, uint8_t grow = 0) {
    auto obj = lv_container_create(parent);
    lv_obj_set_size(obj, width, height);
    if (grow) lv_obj_set_flex_grow(obj, grow);
    return obj;
}

// MARK: Separator
#define UI_SEPARATOR_COLOR_DEFAULT 0x000000

inline lv_obj_t *lv_hor_separator_create(lv_obj_t *parent, lv_color_t color, int32_t margin = 0) {
    auto obj = lv_container_create(parent);
    lv_obj_set_style_bg_color(obj, color, 0);
    lv_obj_set_style_bg_opa(obj, LV_OPA_COVER, 0);
    lv_obj_set_size(obj, LV_PCT(100), 1);
    if (margin) lv_obj_set_style_margin_hor(obj, margin, 0);
    return obj;
}
inline lv_obj_t *lv_hor_separator_create(lv_obj_t *parent, int32_t margin = 0) {
    return lv_hor_separator_create(parent, lv_color_hex(UI_SEPARATOR_COLOR_DEFAULT), margin);
}
inline lv_obj_t *lv_ver_separator_create(lv_obj_t *parent, lv_color_t color, int32_t margin = 0) {
    auto obj = lv_container_create(parent);
    lv_obj_set_style_bg_color(obj, color, 0);
    lv_obj_set_style_bg_opa(obj, LV_OPA_COVER, 0);
    lv_obj_set_size(obj, 1, LV_PCT(100));
    if (margin) lv_obj_set_style_margin_ver(obj, margin, 0);
    return obj;
}
inline lv_obj_t *lv_ver_separator_create(lv_obj_t *parent, int32_t margin = 0) {
    return lv_ver_separator_create(parent, lv_color_hex(UI_SEPARATOR_COLOR_DEFAULT), margin);
}
