/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Shared vocabulary for the BSP and its internal device drivers: geometry
 * primitives, pixel format, esp_err, and the nonnull annotation. This is the
 * one header a device driver needs to speak the BSP's types — it deliberately
 * pulls in nothing heavy (no esp_log / stdio).
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define BSP_NONNULL(...) __attribute__((nonnull(__VA_ARGS__)))

typedef struct { int x, y; } bsp_point_t;
typedef struct { int width, height; } bsp_size_t;

typedef struct { bsp_point_t origin; bsp_size_t size; } bsp_rect_t;
static inline int bsp_rect_width(bsp_rect_t rect) { return rect.size.width; }
static inline int bsp_rect_height(bsp_rect_t rect) { return rect.size.height; }
static inline int bsp_rect_min_x(bsp_rect_t rect) { return rect.origin.x; }
static inline int bsp_rect_min_y(bsp_rect_t rect) { return rect.origin.y; }
static inline int bsp_rect_max_x(bsp_rect_t rect) { return rect.origin.x + rect.size.width; }
static inline int bsp_rect_max_y(bsp_rect_t rect) { return rect.origin.y + rect.size.height; }

typedef enum {
    BSP_DISPLAY_TYPE_NONE,
    BSP_DISPLAY_TYPE_SPI,
    BSP_DISPLAY_TYPE_MIPI_DSI,
    BSP_DISPLAY_TYPE_SPI_EPD,
    BSP_DISPLAY_TYPE_DIRECT_EPD,
} bsp_display_type_t;

typedef enum {
    BSP_PIXEL_FORMAT_L8,
    BSP_PIXEL_FORMAT_RGB565,
    BSP_PIXEL_FORMAT_RGB888,
} bsp_pixel_format_t;

static inline size_t bsp_pixel_format_bytes(bsp_pixel_format_t format) {
    switch (format) {
        case BSP_PIXEL_FORMAT_L8: return 1;
        case BSP_PIXEL_FORMAT_RGB565: return 2;
        case BSP_PIXEL_FORMAT_RGB888: return 3;
        default: return 0;
    }
}

/* Source orientation for bsp_display_draw_bitmap: the pixels are rotated by this
 * much relative to the panel, and the driver un-rotates them into the (panel-
 * coordinate) destination rect while writing. Values mirror lv_display_rotation_t.
 * For 90/270 the source dimensions are the destination rect's, transposed. */
typedef enum {
    BSP_ROTATION_0,
    BSP_ROTATION_90,
    BSP_ROTATION_180,
    BSP_ROTATION_270,
} bsp_rotation_t;

typedef enum {
    BSP_EPD_MODE_NONE,    /* draw() writes framebuffer only, no panel refresh */
    BSP_EPD_MODE_FAST,    /* 2-level direct update, fast                      */
    BSP_EPD_MODE_QUALITY, /* 16 grayscales, flashing, highest quality         */
} bsp_epd_mode_t;

typedef struct {
    int x, y;
    int id;         /*!< pointer track id from the touch controller */
} bsp_touch_point_t;

#ifdef __cplusplus
}
#endif
