/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "TestPatternScreen.hpp"
#include "epd_tweak.hpp"
#include "bsp.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "imgf_dither.h"
#include "imgf_types.h"
#include <cstring>

static const char *TAG = "TestPattern";

static constexpr int kStrips = 16;

TestPatternScreen::~TestPatternScreen() {
    if (fb_) heap_caps_free(fb_);
}

static void draw_strips(uint8_t *dst, int stride, int w, int h) {
    const int sw = w / kStrips;
    for (int i = 0; i < kStrips; i++) {
        const uint8_t v = (uint8_t)(i * 0x11);
        const int x0 = i * sw;
        const int x1 = (i == kStrips - 1) ? w : (i + 1) * sw;
        for (int y = 0; y < h; y++) memset(dst + y * stride + x0, v, x1 - x0);
    }
}

static void draw_gradient(uint8_t *dst, int stride, int w, int h) {
    for (int x = 0; x < w; x++) dst[x] = (uint8_t)((x * 255) / (w - 1));
    for (int y = 1; y < h; y++) memcpy(dst + y * stride, dst, w);
}

static void draw_gradient_ticks(uint8_t *bot, int stride, int w, int h) {
    // Small split ticks (top-black / bottom-white) at the top and bottom edges
    // of the gradient region, at the 16 pure-level X positions x = i*(w-1)/15.
    // Edge-hugging so the gradient body stays clean.
    const int tw = 2, th = 10;
    for (int i = 0; i <= 15; i++) {
        int cx = (int)((long)i * (w - 1) / 15);
        for (int dy = 0; dy < th; dy++) {
            uint8_t color = (dy < th / 2) ? 0x00 : 0xFF;
            for (int dx = 0; dx < tw; dx++) {
                int x = cx - tw / 2 + dx;
                if (x < 0 || x >= w) continue;
                bot[dy * stride + x] = color;
                bot[(h - th + dy) * stride + x] = color;
            }
        }
    }
}

void TestPatternScreen::build() {
    const bsp_size_t size = bsp_display_get_size();
    const int W = size.width, H = size.height;
    const size_t fb_bytes = (size_t)W * H;

    fb_ = (uint8_t *)heap_caps_aligned_alloc(4, fb_bytes, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!fb_) { ESP_LOGE(TAG, "fb alloc (%u bytes)", (unsigned)fb_bytes); return; }

    const int top_h = H / 2;
    const int bot_h = H - top_h;
    uint8_t *bot = fb_ + (size_t)top_h * W;
    draw_strips(fb_, W, W, top_h);
    draw_gradient(bot, W, W, bot_h);

    imgf_dither_opts_t opts = {};
    opts.algo       = IMGF_DITHER_FLOYD_STEINBERG;
    opts.levels     = kStrips;
    opts.out_mode   = IMGF_DITHER_OUT_GRAY;
    opts.serpentine = true;
    imgf_err_t derr = imgf_dither_buffer(bot, (uint16_t)W, (uint16_t)bot_h, (size_t)W,
                                         bot, (size_t)W, &opts);
    if (derr != IMGF_OK) ESP_LOGW(TAG, "dither failed: %s", imgf_err_to_str(derr));

    draw_gradient_ticks(bot, W, W, bot_h);

    dsc_.header.magic  = LV_IMAGE_HEADER_MAGIC;
    dsc_.header.cf     = LV_COLOR_FORMAT_L8;
    dsc_.header.w      = W;
    dsc_.header.h      = H;
    dsc_.header.stride = W;
    dsc_.data_size     = fb_bytes;
    dsc_.data          = fb_;

    lv_obj_t *img = lv_image_create(root_);
    lv_image_set_src(img, &dsc_);
    lv_obj_set_pos(img, 0, 0);

    const int sw = W / kStrips;
    for (int i = 0; i < kStrips; i++) {
        const uint8_t v = (uint8_t)(i * 0x11);
        lv_obj_t *cont = lv_obj_create(root_);
        lv_obj_remove_style_all(cont);
        lv_obj_remove_flag(cont, LV_OBJ_FLAG_CLICKABLE);   // let taps reach root_
        lv_obj_set_pos(cont, i * sw, 0);
        lv_obj_set_size(cont, sw, top_h);

        lv_obj_t *label = lv_label_create(cont);
        lv_label_set_text_fmt(label, "%d", i);
        lv_obj_set_style_text_font(label, &lv_font_montserrat_32, 0);
        lv_obj_set_style_text_color(label, v < 0x80 ? lv_color_white() : lv_color_black(), 0);
        lv_obj_center(label);
    }

    epd_set_next_refresh_mode(BSP_EPD_MODE_QUALITY_ALL);

    // Clear, then invalidate so LVGL re-renders through flush_cb (the redraw's
    // draws block until the clear finishes, so no delay is needed) and the
    // final flush issues QUALITY_ALL via epd_set_next_refresh_mode.
    lv_obj_add_flag(root_, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_event_fn(root_, LV_EVENT_CLICKED, [this](lv_event_t *) {
        bsp_display_clear();
        epd_set_next_refresh_mode(BSP_EPD_MODE_QUALITY_ALL);
        lv_obj_invalidate(root_);
    });
}
