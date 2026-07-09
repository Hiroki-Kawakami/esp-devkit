/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic display layer: holds the active bsp_display provider (registered
 * by the board's bsp_init via bsp_display_set_active) and implements the public
 * bsp_display_* API by dispatching through its vtable. Shared by every board, so
 * a new board only creates a provider — it never re-implements this glue.
 *
 * Optional vtable ops (framebuffers, backlight, EPD mode/refresh) are guarded by
 * a NULL check so the public API stays uniform across panel types: calling
 * bsp_display_refresh() on a MIPI panel, or bsp_display_flush() on an EPD, is a
 * harmless no-op rather than a crash.
 */

#include "bsp.h"
#include "bsp_display.h"
#include <string.h>

static bsp_display_t *s_display;
static void **s_frame_buffers;   /* cached from get_framebuffers() at registration */

void bsp_display_set_active(bsp_display_t *display) {
    s_display = display;
    s_frame_buffers = (display && display->get_framebuffers)
        ? display->get_framebuffers(display)
        : NULL;
}

bsp_display_type_t bsp_display_get_type(void) {
    return s_display ? s_display->type : BSP_DISPLAY_TYPE_NONE;
}

bsp_size_t bsp_display_get_size(void) {
    return s_display ? s_display->size : (bsp_size_t){0, 0};
}

bsp_pixel_format_t bsp_display_get_pixel_format(void) {
    return s_display ? s_display->format : BSP_PIXEL_FORMAT_RGB565;
}

void bsp_display_set_brightness(int brightness) {
    if (s_display && s_display->set_brightness) s_display->set_brightness(s_display, brightness);
}

esp_err_t bsp_display_set_power(bsp_display_power_t state) {
    if (s_display && s_display->set_power) return s_display->set_power(s_display, state);
    return ESP_ERR_NOT_SUPPORTED;
}

void bsp_display_draw_bitmap(bsp_rect_t area, const void *pixels, bsp_rotation_t rotation) {
    if (s_display && s_display->draw_bitmap) s_display->draw_bitmap(s_display, area, pixels, rotation);
}

void bsp_blit_rotated(uint8_t *dst, int dst_stride_px, size_t px,
                      bsp_rect_t area, const void *pixels, bsp_rotation_t rotation) {
    const uint8_t *src = pixels;
    const int w = area.size.width, h = area.size.height;   /* destination rect */
    const int x0 = area.origin.x, y0 = area.origin.y;
    for (int dr = 0; dr < h; dr++) {
        uint8_t *drow = dst + ((size_t)(y0 + dr) * dst_stride_px + x0) * px;
        for (int dc = 0; dc < w; dc++) {
            size_t si;   /* source pixel index (packed, stride = source width) */
            switch (rotation) {
                case BSP_ROTATION_90:  si = (size_t)dc * h + (h - 1 - dr); break;
                case BSP_ROTATION_270: si = (size_t)(w - 1 - dc) * h + dr; break;
                case BSP_ROTATION_180: si = (size_t)(h - 1 - dr) * w + (w - 1 - dc); break;
                default:               si = (size_t)dr * w + dc; break;
            }
            memcpy(drow + (size_t)dc * px, src + si * px, px);
        }
    }
}

void *bsp_display_get_frame_buffer(int fb_index) {
    return s_frame_buffers ? s_frame_buffers[fb_index] : NULL;
}

void bsp_display_flush(int fb_index) {
    if (s_display && s_display->flush) s_display->flush(s_display, fb_index);
}

void bsp_display_set_epd_mode(bsp_epd_mode_t mode) {
    if (s_display && s_display->set_epd_mode) s_display->set_epd_mode(s_display, mode);
}

void bsp_display_refresh(bsp_rect_t area, bsp_epd_mode_t mode) {
    if (s_display && s_display->refresh) s_display->refresh(s_display, area, mode);
}

void bsp_display_clear(void) {
    if (s_display && s_display->clear) s_display->clear(s_display);
}

void bsp_display_wait_idle(void) {
    if (s_display && s_display->wait_idle) s_display->wait_idle(s_display);
}
