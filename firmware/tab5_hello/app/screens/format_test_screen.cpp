/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "format_test_screen.hpp"
#include "display_manager.hpp"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"

static const char *TAG = "format_test";

static constexpr int kOverlayHeight = 240;
static constexpr uint8_t kTestFbNum = BSP_DISPLAY_MAX_FRAME_BUFFERS;

static const char *format_name(bsp_pixel_format_t format) {
    return format == BSP_PIXEL_FORMAT_RGB888 ? "RGB888" : "RGB565";
}

static void put_pixel(uint8_t *pixel, bsp_pixel_format_t format,
                      uint8_t r, uint8_t g, uint8_t b) {
    if (format == BSP_PIXEL_FORMAT_RGB888) {
        pixel[0] = b;
        pixel[1] = g;
        pixel[2] = r;
    } else {
        uint16_t value = (uint16_t)((r >> 3) << 11 | (g >> 2) << 5 | (b >> 3));
        pixel[0] = (uint8_t)value;
        pixel[1] = (uint8_t)(value >> 8);
    }
}

static void fill_gradients(uint8_t *framebuffer, bsp_size_t size,
                           bsp_pixel_format_t format) {
    const size_t bytes = bsp_pixel_format_bytes(format);
    const int bands = 4;
    const int band_height = (size.height - kOverlayHeight) / bands;
    for (int y = 0; y < size.height; y++) {
        const int band = y / band_height;
        uint8_t *row = framebuffer + (size_t)y * size.width * bytes;
        for (int x = 0; x < size.width; x++) {
            const uint8_t v = (uint8_t)(x * 255 / (size.width - 1));
            switch (band) {
                case 0:  put_pixel(row + x * bytes, format, v, 0, 0); break;
                case 1:  put_pixel(row + x * bytes, format, 0, v, 0); break;
                case 2:  put_pixel(row + x * bytes, format, 0, 0, v); break;
                case 3:  put_pixel(row + x * bytes, format, v, v, v); break;
                default: put_pixel(row + x * bytes, format, 0, 0, 0); break;
            }
        }
    }
}

static uint8_t frame_buffer_count() {
    uint8_t count = 0;
    while (count < BSP_DISPLAY_MAX_FRAME_BUFFERS && bsp_display_get_frame_buffer(count)) {
        count++;
    }
    return count;
}

static int64_t reconfigure(bsp_pixel_format_t format, uint8_t fb_num) {
    const int64_t start = esp_timer_get_time();
    esp_err_t err = bsp_display_reconfigure(format, fb_num);
    const int64_t elapsed_ms = (esp_timer_get_time() - start) / 1000;
    ESP_LOGI(TAG, "reconfigure %s x%u: %s (fb %u), %lld ms, psram largest %u",
             format_name(format), fb_num, esp_err_to_name(err),
             frame_buffer_count(), (long long)elapsed_ms,
             (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_SPIRAM));
    return err == ESP_OK ? elapsed_ms : -1;
}

void FormatTestScreen::build() {
    lv_obj_set_style_bg_color(root_, lv_color_black(), 0);
}

void FormatTestScreen::onEnter() {
    main_ = lv_obj_get_display(root_);
    display_manager.set_visible(main_, false);
    bsp_display_wait_draw();

    previous_ = bsp_display_get_pixel_format();
    previous_fb_num_ = frame_buffer_count();
    const bsp_pixel_format_t target = previous_ == BSP_PIXEL_FORMAT_RGB888
        ? BSP_PIXEL_FORMAT_RGB565 : BSP_PIXEL_FORMAT_RGB888;
    const int64_t elapsed_ms = reconfigure(target, kTestFbNum);
    switched_ = elapsed_ms >= 0;

    const bsp_size_t size = bsp_display_get_size();
    const bsp_pixel_format_t format = bsp_display_get_pixel_format();
    for (int i = 0; i < BSP_DISPLAY_MAX_FRAME_BUFFERS; i++) {
        auto *framebuffer = static_cast<uint8_t *>(bsp_display_get_frame_buffer(i));
        if (!framebuffer) break;
        fill_gradients(framebuffer, size, format);
    }

    buildOverlay();
    if (info_) {
        lv_label_set_text_fmt(info_, "%s x%u -> %s x%u  %s  %lld ms  psram largest %u KB",
                              format_name(previous_), previous_fb_num_,
                              format_name(format), frame_buffer_count(),
                              switched_ ? "OK" : "FAILED", (long long)elapsed_ms,
                              (unsigned)(heap_caps_get_largest_free_block(MALLOC_CAP_SPIRAM) / 1024));
    }
}

void FormatTestScreen::onExit() {
    if (overlay_) {
        display_manager.delete_display(overlay_);
        overlay_ = nullptr;
        info_ = nullptr;
    }
    if (switched_) reconfigure(previous_, previous_fb_num_);
    display_manager.set_visible(main_, true);
}

void FormatTestScreen::buildOverlay() {
    const bsp_size_t size = bsp_display_get_size();
    DisplayManagerConfig config = {};
    config.present_mode = DisplayPresentMode::Deferred;
    config.make_default = false;
    config.viewport.output_area = {
        { 0, size.height - kOverlayHeight }, { size.width, kOverlayHeight }
    };
    esp_err_t err = display_manager.create_display(config, &overlay_);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "overlay: %s", esp_err_to_name(err));
        overlay_ = nullptr;
        present();
        return;
    }
    lv_display_add_event_cb(overlay_, [](lv_event_t *e) {
        static_cast<FormatTestScreen *>(lv_event_get_user_data(e))->present();
    }, LV_EVENT_RENDER_READY, this);

    lv_obj_t *screen = lv_display_get_screen_active(overlay_);
    lv_obj_set_style_bg_color(screen, lv_color_hex(0x202020), 0);
    lv_obj_set_flex_flow(screen, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_flex_align(screen, LV_FLEX_ALIGN_CENTER, LV_FLEX_ALIGN_CENTER,
                          LV_FLEX_ALIGN_CENTER);

    info_ = lv_label_create(screen);
    lv_obj_set_style_text_color(info_, lv_color_white(), 0);

    auto button = lv_button_create(screen);
    lv_obj_set_size(button, LV_PCT(80), 96);
    auto title = lv_label_create(button);
    lv_obj_center(title);
    lv_label_set_text(title, "Close");
    lv_obj_add_event_fn(button, LV_EVENT_CLICKED, [](lv_event_t *) {
        lv_async_call([] { screen_manager.pop(); });
    });
}

void FormatTestScreen::present() {
    if (overlay_) display_manager.compose(overlay_, 0);
    display_manager.present(0);
}
