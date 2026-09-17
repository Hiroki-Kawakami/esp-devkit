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
#include "esp_attr.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#ifdef ESP_PLATFORM
#include "esp_cache.h"
#endif

/* The PPA's out buffer is cache-synced by the driver, which needs both the
 * address and the size aligned to a cache line. */
#define PPA_ALIGN_BYTES 64

/* Below this a transaction's setup outweighs the hardware copy. */
#define PPA_MIN_PIXELS 4096

static ppa_client_handle_t s_srm_client;
static SemaphoreHandle_t s_blit_done;
static bool s_blit_pending;
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

void bsp_display_draw_bitmap_async(bsp_rect_t area, const void *pixels, bsp_rotation_t rotation) {
    if (!s_display) return;
    if (s_display->draw_bitmap_async) {
        s_display->draw_bitmap_async(s_display, area, pixels, rotation);
    } else if (s_display->draw_bitmap) {
        s_display->draw_bitmap(s_display, area, pixels, rotation);
    }
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

static IRAM_ATTR bool blit_done_cb(ppa_client_handle_t client, ppa_event_data_t *event,
                                   void *user_data) {
    if (!user_data) return false;
    BaseType_t woken = pdFALSE;
    xSemaphoreGiveFromISR(s_blit_done, &woken);
    return woken == pdTRUE;
}

static bool blit_rotated_ppa(void *dst, bsp_size_t dst_size, bsp_pixel_format_t format,
                             bsp_rect_t area, const void *pixels, bsp_rotation_t rotation,
                             bool async) {
    ppa_srm_color_mode_t color_mode;
    if (!ppa_color_mode(format, &color_mode)) return false;

    const size_t px = bsp_pixel_format_bytes(format);
    const size_t buffer_size = (size_t)dst_size.width * dst_size.height * px;
    if ((size_t)area.size.width * area.size.height < PPA_MIN_PIXELS) return false;
    if ((uintptr_t)dst % PPA_ALIGN_BYTES || buffer_size % PPA_ALIGN_BYTES) return false;

    if (!s_blit_done) {
        s_blit_done = xSemaphoreCreateBinary();
        if (!s_blit_done) return false;
    }
    if (!s_srm_client) {
        ppa_client_config_t config = { .oper_type = PPA_OPERATION_SRM };
        if (ppa_register_client(&config, &s_srm_client) != ESP_OK) return false;
        ppa_event_callbacks_t callbacks = { .on_trans_done = blit_done_cb };
        if (ppa_client_register_event_callbacks(s_srm_client, &callbacks) != ESP_OK) {
            ppa_unregister_client(s_srm_client);
            s_srm_client = NULL;
            return false;
        }
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
        .mode = async ? PPA_TRANS_MODE_NON_BLOCKING : PPA_TRANS_MODE_BLOCKING,
        .user_data = async ? &s_blit_done : NULL,
    };
    operation.in.block_w = operation.in.pic_w;
    operation.in.block_h = operation.in.pic_h;

    if (ppa_do_scale_rotate_mirror(s_srm_client, &operation) != ESP_OK) return false;
    s_blit_pending = async;
    return true;
}
#endif

#if defined(BSP_DISPLAY_USE_PPA) && defined(ESP_PLATFORM)
/* A PPA transaction invalidates its output rows without writing them back, so
 * CPU writes still sitting in the cache would be dropped. */
static void write_back(void *dst, bsp_size_t dst_size, size_t px, bsp_rect_t area) {
    size_t start = ((size_t)area.origin.y * dst_size.width + area.origin.x) * px;
    size_t len = ((size_t)(area.size.height - 1) * dst_size.width + area.size.width) * px;
    esp_cache_msync((uint8_t *)dst + start, len,
                    ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED);
}
#endif

void bsp_blit_wait(void) {
#ifdef BSP_DISPLAY_USE_PPA
    if (!s_blit_pending) return;
    xSemaphoreTake(s_blit_done, portMAX_DELAY);
    s_blit_pending = false;
#endif
}

void bsp_blit_rotated(void *dst, bsp_size_t dst_size, bsp_pixel_format_t format,
                      bsp_rect_t area, const void *pixels, bsp_rotation_t rotation,
                      bool async) {
    bsp_blit_wait();
    if (area.size.width <= 0 || area.size.height <= 0) return;
#ifdef BSP_DISPLAY_USE_PPA
    if ((async || rotation != BSP_ROTATION_0) &&
        blit_rotated_ppa(dst, dst_size, format, area, pixels, rotation, async)) {
        return;
    }
#endif
    const size_t px = bsp_pixel_format_bytes(format);
    if (rotation == BSP_ROTATION_0) {
        const size_t row_bytes = (size_t)area.size.width * px;
        for (int r = 0; r < area.size.height; r++) {
            memcpy((uint8_t *)dst + ((size_t)(area.origin.y + r) * dst_size.width +
                                     area.origin.x) * px,
                   (const uint8_t *)pixels + (size_t)r * row_bytes, row_bytes);
        }
    } else {
        blit_rotated_cpu(dst, dst_size.width, px, area, pixels, rotation);
    }
#if defined(BSP_DISPLAY_USE_PPA) && defined(ESP_PLATFORM)
    write_back(dst, dst_size, px, area);
#endif
}

void *bsp_display_get_frame_buffer(int fb_index) {
    if (!s_frame_buffers || fb_index < 0 ||
        fb_index >= BSP_DISPLAY_MAX_FRAME_BUFFERS) {
        return NULL;
    }
    return s_frame_buffers[fb_index];
}

void bsp_display_flush(int fb_index) {
    if (!s_display || !s_display->flush) return;
    bsp_display_wait_draw();
    s_display->flush(s_display, fb_index);
}

esp_err_t bsp_display_reconfigure(bsp_pixel_format_t pixel_format, uint8_t fb_num) {
    if (!s_display || !s_display->reconfigure) return ESP_ERR_NOT_SUPPORTED;
    bsp_display_wait_draw();
    esp_err_t err = s_display->reconfigure(s_display, pixel_format, fb_num);
    bsp_display_set_active(s_display);
    return err;
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

void bsp_display_wait_draw(void) {
    if (s_display && s_display->wait_draw) s_display->wait_draw(s_display);
}
