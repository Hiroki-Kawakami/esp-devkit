/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GDEY0154D67 EPD display provider (bsp_display_t over the gdey0154d67 driver).
 *
 * Copy-based model, like the other EPD backends: draw_bitmap un-rotates the
 * source into a panel-sized L8 GRAM (one byte per pixel, high = white).
 * refresh() packs the whole GRAM to the panel's native 1bpp (bit 1 = white) and
 * drives a full refresh -- this panel is full-refresh only, so the `area` is
 * ignored (the whole 200x200 frame is always driven). set_epd_mode
 * sets the persistent mode draw_bitmap consults; while it is BSP_EPD_MODE_NONE a
 * draw only updates GRAM. clear blanks to white. Refresh is synchronous (the
 * driver blocks on BUSY).
 */

#include "gdey0154d67_epd.h"
#include "esp_log.h"
#include <string.h>
#include <stdlib.h>

static const char *TAG = "gdey0154d67_epd";

#define PANEL_W  GDEY0154D67_DEFAULT_WIDTH
#define PANEL_H  GDEY0154D67_DEFAULT_HEIGHT
#define WHITE_L8 0xFF

typedef struct {
    bsp_display_t       base;
    gdey0154d67_handle_t epd;
    bsp_epd_mode_t      mode;    /* persistent mode consulted by draw_bitmap */
    uint8_t            *gram;    /* PANEL_W*PANEL_H L8, high = white         */
    uint8_t            *packed;  /* (PANEL_W/8)*PANEL_H 1bpp upload scratch  */
} gdey0154d67_epd_t;

/* GRAM (L8, high = white) -> panel 1bpp (bit 1 = white), MSB = leftmost pixel. */
static void pack_frame(const uint8_t *gram, uint8_t *packed) {
    const int stride = PANEL_W / 8;
    for (int y = 0; y < PANEL_H; y++) {
        const uint8_t *grow = gram + (size_t)y * PANEL_W;
        uint8_t *prow = packed + (size_t)y * stride;
        for (int xb = 0; xb < stride; xb++) {
            uint8_t b = 0;
            for (int bit = 0; bit < 8; bit++) {
                if (grow[xb * 8 + bit] & 0x80) b |= (uint8_t)(0x80 >> bit);
            }
            prow[xb] = b;
        }
    }
}

static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)area;   /* full-refresh only: the whole frame is always driven */
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    const bsp_epd_mode_t m = mode & ~BSP_EPD_MODE_ALL;
    if (m == BSP_EPD_MODE_NONE || m == BSP_EPD_MODE_SEED) return ESP_OK;

    pack_frame(s->gram, s->packed);
    return gdey0154d67_update_full(s->epd, s->packed);
}

static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    const int x0 = area.origin.x, y0 = area.origin.y;
    const int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > PANEL_W || y0 + h > PANEL_H) return ESP_ERR_INVALID_ARG;

    if (rotation == BSP_ROTATION_0) {
        const uint8_t *src = pixels;
        for (int r = 0; r < h; r++)
            memcpy(s->gram + (size_t)(y0 + r) * PANEL_W + x0, src + (size_t)r * w, w);
    } else {
        bsp_blit_rotated(s->gram, PANEL_W, 1, area, pixels, rotation);
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
    memset(s->gram, WHITE_L8, (size_t)PANEL_W * PANEL_H);
    memset(s->packed, 0xFF, (size_t)(PANEL_W / 8) * PANEL_H);   /* all white */
    return gdey0154d67_update_full(s->epd, s->packed);
}

static esp_err_t op_wait_idle(bsp_display_t *self) {
    return gdey0154d67_wait_idle(((gdey0154d67_epd_t *)self)->epd, 0);
}

static esp_err_t op_deinit(bsp_display_t *self) {
    gdey0154d67_epd_t *s = (gdey0154d67_epd_t *)self;
    if (s->epd) gdey0154d67_destroy(s->epd);
    free(s->gram);
    free(s->packed);
    free(s);
    return ESP_OK;
}

esp_err_t gdey0154d67_epd_create(const gdey0154d67_epd_config_t *cfg, bsp_display_t **out_display) {
    if (!cfg || !out_display) return ESP_ERR_INVALID_ARG;

    gdey0154d67_epd_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    s->gram   = malloc((size_t)PANEL_W * PANEL_H);
    s->packed = malloc((size_t)(PANEL_W / 8) * PANEL_H);
    if (!s->gram || !s->packed) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->gram, WHITE_L8, (size_t)PANEL_W * PANEL_H);

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
