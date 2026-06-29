/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "lvgl.hpp"

class TestPatternScreen : public Screen {
public:
    void build() override;
    ~TestPatternScreen() override;

private:
    uint8_t *fb_ = nullptr;
    lv_image_dsc_t dsc_{};
};
