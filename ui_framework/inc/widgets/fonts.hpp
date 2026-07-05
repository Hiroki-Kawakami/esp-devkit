/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "lvgl.h"

// App-injectable fonts for navigation/modal titles and body text. Unset
// (nullptr) falls back to the built-in montserrat_32/24, so a consumer that
// never calls lv_widgets_set_fonts() sees no change.
void lv_widgets_set_fonts(const lv_font_t *title, const lv_font_t *body);
const lv_font_t *lv_widgets_title_font();
const lv_font_t *lv_widgets_body_font();
