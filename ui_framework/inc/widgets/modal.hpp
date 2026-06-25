/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "lvgl.h"

lv_obj_t *lv_modal_open(lv_obj_t *parent);
void lv_modal_close(lv_obj_t *card);
