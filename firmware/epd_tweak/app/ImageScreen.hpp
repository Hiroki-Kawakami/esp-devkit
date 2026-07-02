/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "lvgl.hpp"

// Shows a panel-sized L8 framebuffer received over the serial link. Takes
// ownership of `fb` (heap_caps allocation); tap = clear + QUALITY_ALL re-flash.
class ImageScreen : public Screen {
public:
    explicit ImageScreen(uint8_t *fb) : fb_(fb) {}
    void build() override;
    ~ImageScreen() override;

private:
    uint8_t *fb_;
    lv_image_dsc_t dsc_{};
};
