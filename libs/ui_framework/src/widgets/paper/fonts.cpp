/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets/fonts.hpp"

const lv_font_t *lv_widgets_default_font(lv_widgets_font_role_t role) {
    switch (role) {
#if LV_FONT_MONTSERRAT_32
    case LV_WIDGETS_FONT_TITLE:
    case LV_WIDGETS_FONT_HEADING:
    case LV_WIDGETS_FONT_ICON:
        return &lv_font_montserrat_32;
#endif
#if LV_FONT_MONTSERRAT_24
    case LV_WIDGETS_FONT_BODY:
        return &lv_font_montserrat_24;
#endif
#if LV_FONT_MONTSERRAT_20
    case LV_WIDGETS_FONT_CAPTION:
    case LV_WIDGETS_FONT_BUTTON:
        return &lv_font_montserrat_20;
#endif
    default:
        return nullptr;
    }
}
