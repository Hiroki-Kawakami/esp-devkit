/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"

class LcdTestScreen : public Screen {
public:
    void build() override;
    void onAppear() override;
    void onDisappear() override;

private:
    void apply();
    void adjust(int delta);

    lv_obj_t *brightness_label_;
    int brightness_ = 100;
};
