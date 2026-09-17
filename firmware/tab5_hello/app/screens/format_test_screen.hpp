/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "bsp.h"

class FormatTestScreen : public Screen {
public:
    void build() override;
    void onEnter() override;
    void onExit() override;

private:
    lv_display_t *main_ = nullptr;
    lv_display_t *overlay_ = nullptr;
    lv_obj_t *info_ = nullptr;
    bsp_pixel_format_t previous_ = BSP_PIXEL_FORMAT_RGB565;
    uint8_t previous_fb_num_ = 0;
    bool switched_ = false;

    void buildOverlay();
    void present();
};
