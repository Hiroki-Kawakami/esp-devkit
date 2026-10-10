/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Fonts are resolved per role as lv_widgets_font() -> lv_widgets_default_font()
 * -> nullptr, and a role that resolves to nullptr is left unset so the object
 * inherits the font from its parent.
 *
 * An application overrides lv_widgets_font(). The definition must sit in a
 * translation unit the linker already pulls in for other reasons -- a file
 * holding nothing but the override is never extracted from the application
 * archive.
 */

#pragma once
#include "lvgl.h"

typedef enum lv_widgets_font_role {
    LV_WIDGETS_FONT_TITLE,
    LV_WIDGETS_FONT_HEADING,
    LV_WIDGETS_FONT_BODY,
    LV_WIDGETS_FONT_CAPTION,
    LV_WIDGETS_FONT_BUTTON,
    LV_WIDGETS_FONT_ICON,
    LV_WIDGETS_FONT_COUNT,
} lv_widgets_font_role_t;

const lv_font_t *lv_widgets_font(lv_widgets_font_role_t role);

// nullptr for any role whose montserrat size is not built, so an application
// that supplies every role can disable LV_FONT_MONTSERRAT_* entirely.
const lv_font_t *lv_widgets_default_font(lv_widgets_font_role_t role);

const lv_font_t *lv_widgets_resolved_font(lv_widgets_font_role_t role);
void lv_obj_set_font_role(lv_obj_t *obj, lv_widgets_font_role_t role);
