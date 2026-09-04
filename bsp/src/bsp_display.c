/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic display layer: holds the attached bsp_display providers (the
 * board's built-in panel, registered by its bsp_init, plus any module an app
 * plugs into an expansion port) and implements the public bsp_display_* API by
 * dispatching through the vtable of the panel the caller names. Shared by every
 * board, so a new board only creates a provider — it never re-implements this
 * glue.
 *
 * An unknown panel id and an optional vtable op (framebuffers, backlight, EPD
 * mode/refresh) are both guarded by a NULL check so the public API stays uniform
 * across panel types: calling bsp_display_refresh() on a MIPI panel, or
 * bsp_display_flush() on an EPD, is a harmless no-op rather than a crash.
 */

#include "bsp.h"
#include "bsp_display.h"
#include <string.h>

static bsp_display_t *s_displays[BSP_DISPLAY_MAX_PANELS];
/* cached from get_framebuffers() at attach */
static void         **s_frame_buffers[BSP_DISPLAY_MAX_PANELS];
static int            s_panel_count;

static bsp_display_t *display_of(bsp_panel_id_t panel) {
    if (panel < 0 || panel >= BSP_DISPLAY_MAX_PANELS) return NULL;
    return s_displays[panel];
}

esp_err_t bsp_display_attach(bsp_display_t *display, bsp_panel_id_t *out_panel) {
    if (!display) return ESP_ERR_INVALID_ARG;
    if (s_panel_count >= BSP_DISPLAY_MAX_PANELS) return ESP_ERR_NO_MEM;

    bsp_panel_id_t panel = s_panel_count++;
    s_displays[panel] = display;
    s_frame_buffers[panel] = display->get_framebuffers
        ? display->get_framebuffers(display)
        : NULL;
    if (out_panel) *out_panel = panel;
    return ESP_OK;
}

bsp_display_type_t bsp_display_get_type(bsp_panel_id_t panel) {
    bsp_display_t *display = display_of(panel);
    return display ? display->type : BSP_DISPLAY_TYPE_NONE;
}

uint32_t bsp_display_get_caps(bsp_panel_id_t panel) {
    uint32_t caps = 0;
    bsp_display_t *display = display_of(panel);
    if (!display) return caps;
    if (display->get_framebuffers && display->flush) {
        caps |= BSP_DISPLAY_CAP_FRAMEBUFFER;
    }
    if (display->set_epd_mode && display->refresh) {
        caps |= BSP_DISPLAY_CAP_EPD_REFRESH;
    }
    return caps;
}

bsp_size_t bsp_display_get_size(bsp_panel_id_t panel) {
    bsp_display_t *display = display_of(panel);
    return display ? display->size : (bsp_size_t){0, 0};
}

bsp_pixel_format_t bsp_display_get_pixel_format(bsp_panel_id_t panel) {
    bsp_display_t *display = display_of(panel);
    return display ? display->format : BSP_PIXEL_FORMAT_RGB565;
}

void bsp_display_set_brightness(bsp_panel_id_t panel, int brightness) {
    bsp_display_t *display = display_of(panel);
    if (display && display->set_brightness) display->set_brightness(display, brightness);
}

bsp_rotation_t bsp_display_portrait(bsp_panel_id_t panel) {
    bsp_display_t *display = display_of(panel);
    return display ? display->portrait : BSP_ROTATION_0;
}

esp_err_t bsp_display_set_power(bsp_panel_id_t panel, bsp_display_power_t state) {
    bsp_display_t *display = display_of(panel);
    if (display && display->set_power) return display->set_power(display, state);
    return ESP_ERR_NOT_SUPPORTED;
}

void bsp_display_draw_bitmap(bsp_panel_id_t panel, bsp_rect_t area, const void *pixels,
                             bsp_rotation_t rotation) {
    bsp_display_t *display = display_of(panel);
    if (display && display->draw_bitmap) display->draw_bitmap(display, area, pixels, rotation);
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

void *bsp_display_get_frame_buffer(bsp_panel_id_t panel, int fb_index) {
    if (panel < 0 || panel >= BSP_DISPLAY_MAX_PANELS ||
        !s_frame_buffers[panel] || fb_index < 0 ||
        fb_index >= BSP_DISPLAY_MAX_FRAME_BUFFERS) {
        return NULL;
    }
    return s_frame_buffers[panel][fb_index];
}

void bsp_display_flush(bsp_panel_id_t panel, int fb_index) {
    bsp_display_t *display = display_of(panel);
    if (display && display->flush) display->flush(display, fb_index);
}

void bsp_display_set_epd_mode(bsp_panel_id_t panel, bsp_epd_mode_t mode) {
    bsp_display_t *display = display_of(panel);
    if (display && display->set_epd_mode) display->set_epd_mode(display, mode);
}

void bsp_display_refresh(bsp_panel_id_t panel, bsp_rect_t area, bsp_epd_mode_t mode) {
    bsp_display_t *display = display_of(panel);
    if (display && display->refresh) display->refresh(display, area, mode);
}

void bsp_display_clear(bsp_panel_id_t panel) {
    bsp_display_t *display = display_of(panel);
    if (display && display->clear) display->clear(display);
}

void bsp_display_wait_idle(bsp_panel_id_t panel) {
    bsp_display_t *display = display_of(panel);
    if (display && display->wait_idle) display->wait_idle(display);
}
