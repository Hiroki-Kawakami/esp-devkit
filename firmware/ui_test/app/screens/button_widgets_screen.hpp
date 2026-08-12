/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "widgets.hpp"

class ButtonWidgetsScreen : public NavigationScreen {
public:
    void build() override;

private:
    void create_sample(const char *title, lv_button_type_t type);
};
