/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen.hpp"

lv_obj_t *lv_navigation_create(lv_obj_t *parent);
lv_obj_t *lv_navigation_back_create(lv_obj_t *parent, const char *title,
                                    std::function<void(lv_event_t *)> back);
lv_obj_t *lv_navigation_back_label(lv_obj_t *back);
lv_obj_t *lv_navigation_title_create(lv_obj_t *parent, const char *title);
lv_obj_t *lv_navigation_icon_button_create(lv_obj_t *parent, const lv_font_t *font, const char *icon);

class NavigationScreen : public Screen {
public:
    virtual void build() override = 0;
    virtual void back();

protected:
    lv_obj_t *navigation_;
    lv_obj_t *navigation_title_;
    lv_obj_t *contents_;

    void createNavigation(const char *title, bool back = true);
};
