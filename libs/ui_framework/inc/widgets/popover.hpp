/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "lvgl.h"
#include <functional>

// Opens below anchor, or above it when there is no room. A tap outside the
// card, or deleting the anchor, closes it.
lv_obj_t *lv_popover_open(lv_obj_t *anchor);
void lv_popover_close(lv_obj_t *popover);

lv_obj_t *lv_popover_heading_create(lv_obj_t *popover, const char *text);
lv_obj_t *lv_popover_separator_create(lv_obj_t *popover);
// The popover closes after on_click returns. icon may be nullptr.
lv_obj_t *lv_popover_row_create(lv_obj_t *popover, const char *icon, const lv_font_t *icon_font,
                                const char *text, std::function<void(lv_event_t *)> on_click);
void lv_popover_row_set_checked(lv_obj_t *row, bool checked);
