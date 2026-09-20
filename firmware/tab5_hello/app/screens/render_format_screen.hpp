/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "widgets.hpp"
#include "bsp.h"

class RenderFormatScreen : public NavigationScreen {
public:
    void build() override;
    void onExit() override;

private:
    lv_obj_t *status_ = nullptr;
    bsp_pixel_format_t entry_format_ = BSP_PIXEL_FORMAT_DEFAULT;
    bsp_rotation_t rotation_ = BSP_ROTATION_0;

    void setPanelFormat(bsp_pixel_format_t format);
    void setRenderFormat(lv_color_format_t format);
    void rotate();
    void updateStatus(const char *action, esp_err_t err);
};
