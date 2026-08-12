/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "lvgl.hpp"

typedef uint8_t lv_button_type_t;
#define LV_BUTTON_TYPE_DEFAULT     (0)
#define LV_BUTTON_TYPE_PRIMARY     (1)
#define LV_BUTTON_TYPE_SECONDARY   (2)
#define LV_BUTTON_TYPE_DESTRUCTIVE (3)
#define LV_BUTTON_TYPE_PLAIN       (4)
#define LV_BUTTON_TYPE_OUTLINE     (0x80)

lv_obj_t *lv_button_create(lv_obj_t *parent, lv_button_type_t type);
lv_obj_t *lv_button_set_text(lv_obj_t *button, const char *text, const lv_font_t *font = nullptr);
