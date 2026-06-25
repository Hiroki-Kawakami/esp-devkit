/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "lvgl.h"
#include <functional>

lv_obj_t *lv_modal_open(lv_obj_t *parent);
void lv_modal_close(lv_obj_t *card);

lv_obj_t *lv_modal_title_create(lv_obj_t *parent, const char *text);
lv_obj_t *lv_modal_message_create(lv_obj_t *parent, const char *text);

typedef enum lv_modal_button_type {
    LV_MODAL_BUTTON_TYPE_PRIMARY,
    LV_MODAL_BUTTON_TYPE_SECONDARY,
    LV_MODAL_BUTTON_TYPE_DESTRUCTIVE,
} lv_modal_button_type_t;
lv_obj_t *lv_modal_button_create(lv_obj_t *parent, const char *text, lv_modal_button_type type, std::function<void(lv_event_t*)> on_click);
