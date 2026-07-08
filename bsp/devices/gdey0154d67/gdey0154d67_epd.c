/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GDEY0154D67 EPD display provider (bsp_display_t over the gdey0154d67 driver).
 *
 * The panel is 1-bit B/W, so the frame is held directly as a 1bpp buffer in
 * panel format (bit 1 = white, MSB = leftmost pixel) -- ~5 KB, no L8 GRAM. The
 * display format is still L8 (the BSP has no 1-bit format); draw_bitmap
 * thresholds each incoming L8 pixel into a bit. refresh() drives the whole frame
 * (this driver has no windowed area update): BSP_EPD_MODE_FAST uses the
 * direct-update waveform, everything else the flashing full waveform.
 * set_epd_mode sets the persistent mode draw_bitmap consults; while it is
 * BSP_EPD_MODE_NONE a draw only updates the buffer. clear blanks to white.
 * Refresh is synchronous (the driver blocks on BUSY).
 */

#include "gdey0154d67_epd.h"
#include "esp_log.h"
#include <string.h>
#include <stdlib.h>

static const char *TAG = "gdey0154d67_epd";

#define PANEL_W  GDEY0154D67_DEFAULT_WIDTH
#define PANEL_H  GDEY0154D67_DEFAULT_HEIGHT
#define STRIDE   (PANEL_W / 8)
#define FB_BYTES (STRIDE * PANEL_H)

typedef struct {
    bsp_display_t        base;
    gdey0154d67_handle_t epd;
    bsp_epd_mode_t       mode;   /* persistent mode consulted by draw_bitmap */
    uint8_t             *fb;     /* 1bpp panel format, bit 1 = white         */
} gdey0154d67_epd_t;

static inline void fb_set(uint8_t *fb, int x, int y, bool white) {
    uint8_t *byte = &fb[(size_t)y * STRIDE + (x >> 3)];
    uint8_t  mask = (uint8_t)(0x80 >> (x & 7));
    if (white) *byte |= mask;
    else       *byte &= (uint8_t)~mask;
}

static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)area;   /* whole frame is always driven (no windowed update) */
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    const bsp_epd_mode_t m = mode & ~BSP_EPD_MODE_ALL;
    if (m == BSP_EPD_MODE_NONE || m == BSP_EPD_MODE_SEED) return ESP_OK;

    if (m == BSP_EPD_MODE_FAST && !(mode & BSP_EPD_MODE_ALL))
        return gdey0154d67_update_partial(s->epd, s->fb);
    return gdey0154d67_update_full(s->epd, s->fb);
}

static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    const uint8_t *src = pixels;
    const int x0 = area.origin.x, y0 = area.origin.y;
    const int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > PANEL_W || y0 + h > PANEL_H) return ESP_ERR_INVALID_ARG;

    /* Source index math matches bsp_blit_rotated; write a thresholded bit. */
    for (int dr = 0; dr < h; dr++) {
        for (int dc = 0; dc < w; dc++) {
            size_t si;
            switch (rotation) {
                case BSP_ROTATION_90:  si = (size_t)dc * h + (h - 1 - dr); break;
                case BSP_ROTATION_270: si = (size_t)(w - 1 - dc) * h + dr; break;
                case BSP_ROTATION_180: si = (size_t)(h - 1 - dr) * w + (w - 1 - dc); break;
                default:               si = (size_t)dr * w + dc; break;
            }
            fb_set(s->fb, x0 + dc, y0 + dr, src[si] & 0x80);
        }
    }

    const bsp_epd_mode_t mode = s->mode & ~BSP_EPD_MODE_ALL;
    if (mode != BSP_EPD_MODE_NONE && mode != BSP_EPD_MODE_SEED)
        return self->refresh(self, area, s->mode);
    return ESP_OK;
}

static esp_err_t op_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    ((gdey0154d67_epd_t *)self)->mode = mode;
    return ESP_OK;
}

static esp_err_t op_clear(bsp_display_t *self) {
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    memset(s->fb, 0xFF, FB_BYTES);   /* all white */
    return gdey0154d67_update_full(s->epd, s->fb);
}

static esp_err_t op_wait_idle(bsp_display_t *self) {
    return gdey0154d67_wait_idle(((gdey0154d67_epd_t *)self)->epd, 0);
}

static esp_err_t op_deinit(bsp_display_t *self) {
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    if (s->epd) gdey0154d67_destroy(s->epd);
    free(s->fb);
    free(s);
    return ESP_OK;
}

esp_err_t gdey0154d67_epd_create(const gdey0154d67_epd_config_t *cfg, bsp_display_t **out_display) {
    if (!cfg || !out_display) return ESP_ERR_INVALID_ARG;

    gdey0154d67_epd_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    s->fb = malloc(FB_BYTES);
    if (!s->fb) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->fb, 0xFF, FB_BYTES);   /* white */

    const gdey0154d67_config_t drv_cfg = {
        .spi_host = cfg->spi_host,
        .cs_io    = cfg->cs_io,
        .dc_io    = cfg->dc_io,
        .reset_io = cfg->reset_io,
        .busy_io  = cfg->busy_io,
        .clock_hz = cfg->clock_hz,
        .width    = PANEL_W,
        .height   = PANEL_H,
    };
    esp_err_t err = gdey0154d67_create(&drv_cfg, &s->epd);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gdey0154d67_create: %s", esp_err_to_name(err));
        op_deinit(&s->base);
        return err;
    }

    s->base.type         = BSP_DISPLAY_TYPE_SPI_EPD;
    s->base.size         = (bsp_size_t){ PANEL_W, PANEL_H };
    s->base.format       = BSP_PIXEL_FORMAT_L8;
    s->base.draw_bitmap  = op_draw_bitmap;
    s->base.deinit       = op_deinit;
    s->base.set_epd_mode = op_set_epd_mode;
    s->base.refresh      = op_refresh;
    s->base.clear        = op_clear;
    s->base.wait_idle    = op_wait_idle;
    s->mode              = BSP_EPD_MODE_NONE;

    *out_display = &s->base;
    return ESP_OK;
}
