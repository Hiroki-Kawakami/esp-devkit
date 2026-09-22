/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets/fonts.hpp"

__attribute__((weak)) const lv_font_t *lv_widgets_font(lv_widgets_font_role_t) {
    return nullptr;
}

__attribute__((weak)) const lv_font_t *lv_widgets_default_font(lv_widgets_font_role_t) {
    return nullptr;
}

const lv_font_t *lv_widgets_resolved_font(lv_widgets_font_role_t role) {
    const lv_font_t *font = lv_widgets_font(role);
    return font ? font : lv_widgets_default_font(role);
}

void lv_obj_set_font_role(lv_obj_t *obj, lv_widgets_font_role_t role) {
    const lv_font_t *font = lv_widgets_resolved_font(role);
    if (font) lv_obj_set_style_text_font(obj, font, 0);
}
