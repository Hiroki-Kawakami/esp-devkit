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
#include <stdint.h>
#include <string.h>

#ifdef ESP_PLATFORM
#include "soc/soc_caps.h"
#if SOC_PPA_SUPPORTED
#define BSP_DISPLAY_USE_PPA 1
#endif
#else
#define BSP_DISPLAY_USE_PPA 1
#endif

#ifdef BSP_DISPLAY_USE_PPA
#include "driver/ppa.h"

/* The PPA's out buffer is cache-synced by the driver, which needs both the
 * address and the size aligned to a cache line. */
#define PPA_ALIGN_BYTES 64

/* Below this a transaction's setup outweighs the hardware copy. */
#define PPA_MIN_PIXELS 4096
#endif

static bsp_display_t *s_display;
static void **s_frame_buffers;   /* cached from get_framebuffers() at registration */

void bsp_display_set_active(bsp_display_t *display) {
    s_display = display;
    s_frame_buffers = (display && display->get_framebuffers)
        ? display->get_framebuffers(display)
        : NULL;
}

bsp_display_t *bsp_display_get_active(void) {
    return s_display;
}

bsp_display_type_t bsp_display_get_type(void) {
    return s_display ? s_display->type : BSP_DISPLAY_TYPE_NONE;
}

uint32_t bsp_display_get_caps(void) {
    uint32_t caps = 0;
    if (!s_display) return caps;
    if (s_display->get_framebuffers && s_display->flush) {
        caps |= BSP_DISPLAY_CAP_FRAMEBUFFER;
    }
    if (s_display->set_epd_mode && s_display->refresh) {
        caps |= BSP_DISPLAY_CAP_EPD_REFRESH;
    }
    if (s_display->read_bitmap) {
        caps |= BSP_DISPLAY_CAP_READBACK;
    }
    return caps;
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

__attribute__((weak)) bsp_rotation_t bsp_display_portrait(void) {
    return BSP_ROTATION_0;
}

esp_err_t bsp_display_set_power(bsp_display_power_t state) {
    if (s_display && s_display->set_power) return s_display->set_power(s_display, state);
    return ESP_ERR_NOT_SUPPORTED;
}

void bsp_display_draw_bitmap(bsp_rect_t area, const void *pixels, bsp_rotation_t rotation) {
    if (s_display && s_display->draw_bitmap) s_display->draw_bitmap(s_display, area, pixels, rotation);
}

static void blit_rotated_cpu(uint8_t *dst, int dst_stride_px, size_t px,
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

#ifdef BSP_DISPLAY_USE_PPA
static bool ppa_color_mode(bsp_pixel_format_t format, ppa_srm_color_mode_t *out) {
    switch (format) {
        case BSP_PIXEL_FORMAT_RGB565: *out = PPA_SRM_COLOR_MODE_RGB565; return true;
        case BSP_PIXEL_FORMAT_RGB888: *out = PPA_SRM_COLOR_MODE_RGB888; return true;
        default:                      return false;
    }
}

static ppa_srm_rotation_angle_t ppa_angle(bsp_rotation_t rotation) {
    switch (rotation) {
        case BSP_ROTATION_90:  return PPA_SRM_ROTATION_ANGLE_90;
        case BSP_ROTATION_180: return PPA_SRM_ROTATION_ANGLE_180;
        case BSP_ROTATION_270: return PPA_SRM_ROTATION_ANGLE_270;
        default:               return PPA_SRM_ROTATION_ANGLE_0;
    }
}

static bool blit_rotated_ppa(void *dst, bsp_size_t dst_size, bsp_pixel_format_t format,
                             bsp_rect_t area, const void *pixels, bsp_rotation_t rotation) {
    static ppa_client_handle_t s_srm_client;

    ppa_srm_color_mode_t color_mode;
    if (!ppa_color_mode(format, &color_mode)) return false;

    const size_t px = bsp_pixel_format_bytes(format);
    const size_t buffer_size = (size_t)dst_size.width * dst_size.height * px;
    if ((size_t)area.size.width * area.size.height < PPA_MIN_PIXELS) return false;
    if ((uintptr_t)dst % PPA_ALIGN_BYTES || buffer_size % PPA_ALIGN_BYTES) return false;

    if (!s_srm_client) {
        ppa_client_config_t config = { .oper_type = PPA_OPERATION_SRM };
        if (ppa_register_client(&config, &s_srm_client) != ESP_OK) return false;
    }

    const bool transposed = rotation == BSP_ROTATION_90 || rotation == BSP_ROTATION_270;
    ppa_srm_oper_config_t operation = {
        .in = {
            .buffer   = pixels,
            .pic_w    = transposed ? (uint32_t)area.size.height : (uint32_t)area.size.width,
            .pic_h    = transposed ? (uint32_t)area.size.width : (uint32_t)area.size.height,
            .srm_cm   = color_mode,
        },
        .out = {
            .buffer          = dst,
            .buffer_size     = (uint32_t)buffer_size,
            .pic_w           = (uint32_t)dst_size.width,
            .pic_h           = (uint32_t)dst_size.height,
            .block_offset_x  = (uint32_t)area.origin.x,
            .block_offset_y  = (uint32_t)area.origin.y,
            .srm_cm          = color_mode,
        },
        .rotation_angle = ppa_angle(rotation),
        .scale_x = 1.0f,
        .scale_y = 1.0f,
        .mode = PPA_TRANS_MODE_BLOCKING,
    };
    operation.in.block_w = operation.in.pic_w;
    operation.in.block_h = operation.in.pic_h;

    return ppa_do_scale_rotate_mirror(s_srm_client, &operation) == ESP_OK;
}
#endif

void bsp_blit_rotated(void *dst, bsp_size_t dst_size, bsp_pixel_format_t format,
                      bsp_rect_t area, const void *pixels, bsp_rotation_t rotation) {
    const size_t px = bsp_pixel_format_bytes(format);
    if (rotation == BSP_ROTATION_0) {
        const size_t row_bytes = (size_t)area.size.width * px;
        for (int r = 0; r < area.size.height; r++) {
            memcpy((uint8_t *)dst + ((size_t)(area.origin.y + r) * dst_size.width +
                                     area.origin.x) * px,
                   (const uint8_t *)pixels + (size_t)r * row_bytes, row_bytes);
        }
        return;
    }
#ifdef BSP_DISPLAY_USE_PPA
    if (blit_rotated_ppa(dst, dst_size, format, area, pixels, rotation)) return;
#endif
    blit_rotated_cpu(dst, dst_size.width, px, area, pixels, rotation);
}

void *bsp_display_get_frame_buffer(int fb_index) {
    if (!s_frame_buffers || fb_index < 0 ||
        fb_index >= BSP_DISPLAY_MAX_FRAME_BUFFERS) {
        return NULL;
    }
    return s_frame_buffers[fb_index];
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
