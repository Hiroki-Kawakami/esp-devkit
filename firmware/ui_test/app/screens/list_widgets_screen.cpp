/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "list_widgets_screen.hpp"

#include <cstdio>

void ListWidgetsScreen::build() {
    createNavigation("List Widget", LV_NAVIGATION_STYLE_LIST | LV_NAVIGATION_STYLE_BACK);
    lv_obj_remove_flag(root_, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_remove_flag(contents_, LV_OBJ_FLAG_SCROLLABLE);
    list_ = lv_list_create(contents_, this);
}

std::size_t ListWidgetsScreen::rowCount() const {
    return kRowCount;
}

int32_t ListWidgetsScreen::rowHeight() const {
    return 77;
}

lv_obj_t *ListWidgetsScreen::createRow(lv_obj_t *parent) {
    lv_obj_t *row = lv_list_row_create(parent);
    lv_obj_set_style_pad_hor(row, 20, 0);

    lv_obj_t *label = lv_label_create(row);
    lv_obj_set_style_text_font(label, lv_widgets_body_font(), 0);
    lv_obj_align(label, LV_ALIGN_LEFT_MID, 0, 0);
    lv_obj_set_user_data(row, label);
    return row;
}

void ListWidgetsScreen::bindRow(lv_obj_t *row, std::size_t index) {
    lv_obj_t *label = static_cast<lv_obj_t *>(lv_obj_get_user_data(row));
    char text[48];
    if (index == selected_) {
        snprintf(text, sizeof(text), "Row %03u  (selected)", static_cast<unsigned>(index + 1));
    } else {
        snprintf(text, sizeof(text), "Row %03u", static_cast<unsigned>(index + 1));
    }
    lv_label_set_text(label, text);
}

void ListWidgetsScreen::didSelectRow(std::size_t index) {
    selected_ = index;
    lv_list_reload_data(list_, true);
}

std::string ListWidgetsScreen::statusBarText(lv_obj_t *, int page, int total_pages) {
    char text[48];
    snprintf(text, sizeof(text), "%u items  %d / %d",
             static_cast<unsigned>(rowCount()), page, total_pages);
    return text;
}
