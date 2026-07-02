/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * IT8951E EPD display provider (bsp_display_t over the it8951e TCON driver).
 *
 * Model: draw_bitmap un-rotates the source into a panel-sized L8 GRAM (one byte
 * per pixel, high nibble = gray), exactly like the other copy-based EPD
 * backends. refresh(area, mode) packs that area to the panel's native 4bpp,
 * uploads it to the TCON (it8951e_load_image), and drives the waveform
 * (it8951e_display), then blocks until the refresh completes. set_epd_mode sets
 * the persistent mode draw_bitmap consults; while it is BSP_EPD_MODE_NONE a draw
 * only updates GRAM. The GRAM lets refresh re-push the latest contents and lets
 * the 4bpp x/width alignment snap outward without losing edge pixels.
 *
 * The reported size is the IT8951E's reported panel geometry, which on M5Paper
 * is 960x540 landscape — matching ED047TC1/M5PaperS3 so the shared app renders
 * identically. If a panel comes up rotated/mirrored, fix it here (the
 * it8951e_image_t rotation/endian or the size transpose) — orientation is the
 * one bit that needs confirming on hardware.
 */

#include "it8951e_epd.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include <string.h>
#include <stdlib.h>

static const char *TAG = "it8951e_epd";

#define REFRESH_TIMEOUT_MS 30000

typedef struct {
    bsp_display_t    base;
    it8951e_handle_t epd;
    uint16_t         panel_w;
    uint16_t         panel_h;
    bsp_epd_mode_t   mode;     /* persistent mode consulted by draw_bitmap */
    uint8_t         *gram;     /* panel_w*panel_h L8, high nibble = gray    */
    uint8_t         *packed;   /* 4bpp upload scratch, panel_w*panel_h/2    */
} it8951e_epd_t;

/* The TCON does its own differential drive, so BSP_EPD_MODE_ALL only picks the
 * waveform here -- the whole area is uploaded and driven either way. */
static it8951e_mode_t to_it8951e_mode(bsp_epd_mode_t m) {
    switch (m & ~BSP_EPD_MODE_ALL) {
        case BSP_EPD_MODE_FAST:    return IT8951E_MODE_DU;
        case BSP_EPD_MODE_QUALITY: return IT8951E_MODE_GC16;
        default:                   return IT8951E_MODE_GL16;
    }
}

static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    it8951e_epd_t *s = (it8951e_epd_t *)self;
    const int W = s->panel_w, H = s->panel_h;
    const int x0 = area.origin.x, y0 = area.origin.y;
    const int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > W || y0 + h > H) return ESP_ERR_INVALID_ARG;

    if (rotation == BSP_ROTATION_0) {
        const uint8_t *src = pixels;
        for (int r = 0; r < h; r++)
            memcpy(s->gram + (size_t)(y0 + r) * W + x0, src + (size_t)r * w, w);
    } else {
        bsp_blit_rotated(s->gram, W, 1, area, pixels, rotation);
    }

    if ((s->mode & ~BSP_EPD_MODE_ALL) != BSP_EPD_MODE_NONE)
        return self->refresh(self, area, s->mode);
    return ESP_OK;
}

static esp_err_t op_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    ((it8951e_epd_t *)self)->mode = mode;
    return ESP_OK;
}

/* Blank the whole panel to white via the TCON's INIT flush. */
static esp_err_t op_clear(bsp_display_t *self) {
    it8951e_epd_t *s = (it8951e_epd_t *)self;
    memset(s->gram, 0xF0, (size_t)s->panel_w * s->panel_h);   /* white */
    esp_err_t err = it8951e_bus_acquire(s->epd);
    if (err != ESP_OK) return err;
    err = it8951e_clear(s->epd);
    it8951e_bus_release(s->epd);
    if (err == ESP_OK) err = it8951e_wait_idle(s->epd, REFRESH_TIMEOUT_MS);
    return err;
}

static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    it8951e_epd_t *s = (it8951e_epd_t *)self;
    if ((mode & ~BSP_EPD_MODE_ALL) == BSP_EPD_MODE_NONE) return ESP_OK;

    /* IT8951E 4bpp upload requires x and width aligned to 4 px. Snap the rect
     * outward; the padding columns are read back from GRAM, which draw_bitmap
     * keeps coherent. panel_w (960) is a multiple of 4, so the right edge clamp
     * stays aligned. */
    int x0 = area.origin.x & ~3;
    int x1 = (area.origin.x + area.size.width + 3) & ~3;
    if (x1 > s->panel_w) x1 = s->panel_w;
    int y0 = area.origin.y;
    int y1 = area.origin.y + area.size.height;
    if (x1 <= x0 || y1 <= y0) return ESP_OK;
    const int aw = x1 - x0, ah = y1 - y0;

    uint8_t *dst = s->packed;
    for (int r = 0; r < ah; r++) {
        const uint8_t *grow = s->gram + (size_t)(y0 + r) * s->panel_w + x0;
        for (int c = 0; c < aw; c += 2)
            *dst++ = (uint8_t)((grow[c] & 0xF0) | (grow[c + 1] >> 4));
    }

    const it8951e_area_t it_area = {
        (uint16_t)x0, (uint16_t)y0, (uint16_t)aw, (uint16_t)ah,
    };
    const it8951e_image_t img = {
        .bpp       = IT8951E_BPP_4,
        .rotation  = IT8951E_ROT_0,
        .endian    = IT8951E_ENDIAN_BIG,
        .data      = s->packed,
        .data_size = (size_t)aw * ah / 2,
    };
    /* Hold the shared bus across the SPI-active upload+display so an SD read on
     * the same bus can't interleave between packets; release before wait_idle so
     * the bus stays free while the panel physically refreshes. */
    esp_err_t err = it8951e_bus_acquire(s->epd);
    if (err != ESP_OK) return err;
    err = it8951e_load_image(s->epd, &it_area, &img);
    if (err == ESP_OK) err = it8951e_display(s->epd, &it_area, to_it8951e_mode(mode));
    it8951e_bus_release(s->epd);
    if (err == ESP_OK) err = it8951e_wait_idle(s->epd, REFRESH_TIMEOUT_MS);
    return err;
}

static esp_err_t op_deinit(bsp_display_t *self) {
    it8951e_epd_t *s = (it8951e_epd_t *)self;
    if (s->epd) it8951e_destroy(s->epd);
    free(s->gram);
    free(s->packed);
    free(s);
    return ESP_OK;
}

esp_err_t it8951e_epd_create(const it8951e_config_t *cfg, bsp_display_t **out_display) {
    if (!cfg || !out_display) return ESP_ERR_INVALID_ARG;

    it8951e_epd_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    esp_err_t err = it8951e_create(cfg, &s->epd);
    if (err != ESP_OK) { free(s); return err; }

    it8951e_panel_info_t info;
    err = it8951e_get_panel_info(s->epd, &info);
    if (err != ESP_OK) { op_deinit(&s->base); return err; }
    s->panel_w = info.panel_w;
    s->panel_h = info.panel_h;
    ESP_LOGI(TAG, "panel %ux%u, fw=%s lut=%s",
             info.panel_w, info.panel_h, info.fw_version, info.lut_version);

    const size_t px = (size_t)s->panel_w * s->panel_h;
    s->gram   = heap_caps_malloc(px,     MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    s->packed = heap_caps_malloc(px / 2, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!s->gram || !s->packed) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->gram, 0xF0, px);   /* white */

    s->base.type         = BSP_DISPLAY_TYPE_SPI_EPD;
    s->base.size         = (bsp_size_t){ s->panel_w, s->panel_h };
    s->base.format       = BSP_PIXEL_FORMAT_L8;
    s->base.draw_bitmap  = op_draw_bitmap;
    s->base.deinit       = op_deinit;
    s->base.set_epd_mode = op_set_epd_mode;
    s->base.refresh      = op_refresh;
    s->base.clear        = op_clear;
    s->mode              = BSP_EPD_MODE_NONE;

    *out_display = &s->base;
    return ESP_OK;
}
