/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen.hpp"

typedef uint8_t lv_navigation_style_t;
#define LV_NAVIGATION_STYLE_DEFAULT (0)
#define LV_NAVIGATION_STYLE_UNIFIED (1)
#define LV_NAVIGATION_STYLE_LIST    (2)
#define LV_NAVIGATION_STYLE_BACK    (0x80)

lv_obj_t *lv_navigation_create(lv_obj_t *parent, lv_navigation_style_t style = LV_NAVIGATION_STYLE_DEFAULT);
lv_obj_t *lv_navigation_back_create(lv_obj_t *parent, const char *title, std::function<void(lv_event_t *)> back);
lv_obj_t *lv_navigation_back_label(lv_obj_t *back);
lv_obj_t *lv_navigation_title_create(lv_obj_t *parent, const char *title);

class NavigationScreen : public Screen {
public:
    virtual void build() override = 0;
    virtual void back();

protected:
    lv_obj_t *navigation_;
    lv_obj_t *navigation_title_;
    lv_obj_t *contents_;

    void createNavigation(const char *title, lv_navigation_style_t style = LV_NAVIGATION_STYLE_DEFAULT);
};
