/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ImageScreen.hpp"
#include "epd_tweak.hpp"
#include "bsp.h"
#include "esp_heap_caps.h"

ImageScreen::~ImageScreen() {
    if (fb_) heap_caps_free(fb_);
}

void ImageScreen::build() {
    const bsp_size_t size = bsp_display_get_size();

    dsc_.header.magic  = LV_IMAGE_HEADER_MAGIC;
    dsc_.header.cf     = LV_COLOR_FORMAT_L8;
    dsc_.header.w      = size.width;
    dsc_.header.h      = size.height;
    dsc_.header.stride = size.width;
    dsc_.data_size     = (size_t)size.width * size.height;
    dsc_.data          = fb_;

    lv_obj_t *img = lv_image_create(root_);
    lv_image_set_src(img, &dsc_);
    lv_obj_set_pos(img, 0, 0);

    epd_set_next_refresh_mode(BSP_EPD_MODE_QUALITY_ALL);

    lv_obj_add_flag(root_, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_fn(root_, LV_EVENT_CLICKED, [this](lv_event_t *) {
        bsp_display_clear();
        epd_set_next_refresh_mode(BSP_EPD_MODE_QUALITY_ALL);
        lv_obj_invalidate(root_);
    });
}
