/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "screen.hpp"

lv_obj_t *lv_navigation_create(lv_obj_t *parent);
lv_obj_t *lv_navigation_back_create(lv_obj_t *parent, const char *title, std::function<void()> back);
lv_obj_t *lv_navigation_back_label(lv_obj_t *back);

class NavigationScreen : public Screen {
public:
    virtual void build() override = 0;
    virtual void back();

protected:
    lv_obj_t *navigation_;
    lv_obj_t *navigation_title_;
    lv_obj_t *contents_;

    void createNavigation(const char *title);
};
