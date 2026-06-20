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

void bsp_display_draw_bitmap(bsp_rect_t area, const void *pixels) {
    if (s_display && s_display->draw_bitmap) s_display->draw_bitmap(s_display, area, pixels);
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
