/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets/fonts.hpp"

namespace {
const lv_font_t *s_title_font;
const lv_font_t *s_body_font;
}  // namespace

void lv_widgets_set_fonts(const lv_font_t *title, const lv_font_t *body) {
    s_title_font = title;
    s_body_font = body;
}

const lv_font_t *lv_widgets_title_font() { return s_title_font ? s_title_font : &lv_font_montserrat_32; }
const lv_font_t *lv_widgets_body_font()  { return s_body_font  ? s_body_font  : &lv_font_montserrat_24; }
