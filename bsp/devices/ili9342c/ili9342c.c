/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ili9342c.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "ili9342c";

#define CHUNK_PX  (ILI9342C_DMA_CHUNK_BYTES / 2)   /* RGB565 pixels per DMA chunk */
#define NBUF      2                                /* ping-pong DMA bounce buffers */

/* Commands. */
#define CMD_SWRESET  0x01
#define CMD_SLPIN    0x10
#define CMD_SLPOUT   0x11
#define CMD_INVOFF   0x20
#define CMD_INVON    0x21
#define CMD_NORON    0x13
#define CMD_DISPOFF  0x28
#define CMD_DISPON   0x29
#define CMD_CASET    0x2A
#define CMD_RASET    0x2B
#define CMD_RAMWR    0x2C
#define CMD_COLMOD   0x3A
#define CMD_MADCTL   0x36

typedef struct {
    bsp_display_t base;
    spi_device_handle_t spi;
    gpio_num_t dc_io;
    uint8_t    madctl;
    bool       invert;
    bsp_display_power_t power;
    ili9342c_reset_cb_t     set_reset;
    void      *reset_ctx;
    ili9342c_backlight_cb_t set_backlight;
    void      *backlight_ctx;
    ili9342c_power_cb_t     set_panel_power;
    void      *panel_power_ctx;

    uint16_t *dma_buf[NBUF];
    spi_transaction_t trans[NBUF];
    uint16_t *rotate_buf;
    size_t    rotate_px;
} ili9342c_t;

static inline void dc(ili9342c_t *d, int level) { gpio_set_level(d->dc_io, level); }

static void tx(ili9342c_t *d, const uint8_t *data, size_t len) {
    spi_transaction_t t = { .length = len * 8, .tx_buffer = data };
    spi_device_polling_transmit(d->spi, &t);
}

static void write_cmd(ili9342c_t *d, uint8_t c) { dc(d, 0); tx(d, &c, 1); }
static void write_data(ili9342c_t *d, const uint8_t *data, size_t len) { dc(d, 1); tx(d, data, len); }

static void set_window(ili9342c_t *d, int x0, int y0, int x1, int y1) {
    uint8_t col[4] = { x0 >> 8, x0 & 0xFF, x1 >> 8, x1 & 0xFF };
    uint8_t row[4] = { y0 >> 8, y0 & 0xFF, y1 >> 8, y1 & 0xFF };
    write_cmd(d, CMD_CASET); write_data(d, col, 4);
    write_cmd(d, CMD_RASET); write_data(d, row, 4);
    write_cmd(d, CMD_RAMWR);
}

/* Stream `count` RGB565 pixels to GRAM, byte-swapping to the panel's big-endian
 * order into DMA bounce buffers and overlapping the swap of the next chunk with
 * the DMA of the current one. */
static void stream_pixels(ili9342c_t *d, const uint16_t *px, size_t count) {
    dc(d, 1);
    size_t queued = 0, idx = 0;
    int bi = 0;
    while (idx < count) {
        if (queued == NBUF) {
            spi_transaction_t *r;
            spi_device_get_trans_result(d->spi, &r, portMAX_DELAY);
            queued--;
        }
        size_t n = count - idx < CHUNK_PX ? count - idx : CHUNK_PX;
        uint16_t *b = d->dma_buf[bi];
        for (size_t i = 0; i < n; i++) b[i] = __builtin_bswap16(px[idx + i]);
        d->trans[bi] = (spi_transaction_t){ .length = n * 16, .tx_buffer = b };
        spi_device_queue_trans(d->spi, &d->trans[bi], portMAX_DELAY);
        queued++;
        bi ^= 1;
        idx += n;
    }
    while (queued) {
        spi_transaction_t *r;
        spi_device_get_trans_result(d->spi, &r, portMAX_DELAY);
        queued--;
    }
}

static esp_err_t draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                             bsp_rotation_t rotation) {
    ili9342c_t *d = (ili9342c_t *)self;
    const int w = area.size.width, h = area.size.height;
    if (w <= 0 || h <= 0) return ESP_OK;

    const uint16_t *src = pixels;
    if (rotation != BSP_ROTATION_0) {
        size_t need = (size_t)w * h;
        if (d->rotate_px < need) {
            free(d->rotate_buf);
            d->rotate_buf = malloc(need * 2);
            d->rotate_px = d->rotate_buf ? need : 0;
        }
        if (!d->rotate_buf) return ESP_ERR_NO_MEM;
        bsp_blit_rotated((uint8_t *)d->rotate_buf, w, 2,
                         (bsp_rect_t){ {0, 0}, { w, h } }, pixels, rotation);
        src = d->rotate_buf;
    }

    set_window(d, area.origin.x, area.origin.y,
               area.origin.x + w - 1, area.origin.y + h - 1);
    stream_pixels(d, src, (size_t)w * h);
    return ESP_OK;
}

static esp_err_t set_brightness(bsp_display_t *self, int brightness) {
    ili9342c_t *d = (ili9342c_t *)self;
    d->set_backlight(d->backlight_ctx, brightness);
    return ESP_OK;
}

static esp_err_t deinit(bsp_display_t *self) {
    ili9342c_t *d = (ili9342c_t *)self;
    if (d->spi) spi_bus_remove_device(d->spi);
    for (int i = 0; i < NBUF; i++) heap_caps_free(d->dma_buf[i]);
    free(d->rotate_buf);
    free(d);
    return ESP_OK;
}

static void hw_reset(ili9342c_t *d) {
    if (!d->set_reset) {
        write_cmd(d, CMD_SWRESET);
        vTaskDelay(pdMS_TO_TICKS(120));
        return;
    }
    d->set_reset(d->reset_ctx, false);   vTaskDelay(pdMS_TO_TICKS(10));
    d->set_reset(d->reset_ctx, true);    vTaskDelay(pdMS_TO_TICKS(10));   /* assert */
    d->set_reset(d->reset_ctx, false);   vTaskDelay(pdMS_TO_TICKS(120));
}

/* ILI9342C power/gamma bring-up. 0xC8 (SETEXTC 0xFF,0x93,0x42) unlocks the
 * vendor commands -- this is what distinguishes ILI9342C from its ILI9341
 * sibling. Each entry is {cmd, len, data...}; 0xFF marks the end. */
static const uint8_t INIT_SEQ[] = {
    0xC8, 3, 0xFF, 0x93, 0x42,           /* SETEXTC: enable vendor commands */
    0xC0, 2, 0x12, 0x12,                 /* power control 1 */
    0xC1, 1, 0x03,                       /* power control 2 */
    0xC5, 1, 0xF2,                       /* VCOM control */
    0xB0, 1, 0xE0,                       /* interface mode control */
    0xF6, 3, 0x01, 0x00, 0x00,           /* interface control */
    0xE0, 15, 0x00, 0x0C, 0x11, 0x04, 0x11, 0x08, 0x37, 0x89,
              0x4C, 0x06, 0x0C, 0x0A, 0x2E, 0x34, 0x0F,   /* positive gamma */
    0xE1, 15, 0x00, 0x0B, 0x11, 0x05, 0x13, 0x09, 0x33, 0x67,
              0x48, 0x07, 0x0E, 0x0B, 0x2E, 0x33, 0x0F,   /* negative gamma */
    0xB6, 4, 0x08, 0x82, 0x1D, 0x04,     /* display function control */
    0xFF,
};

static void panel_init(ili9342c_t *d) {
    hw_reset(d);
    write_cmd(d, CMD_SLPOUT);  vTaskDelay(pdMS_TO_TICKS(120));
    for (const uint8_t *p = INIT_SEQ; p[0] != 0xFF; p += 2 + p[1]) {
        write_cmd(d, p[0]);
        if (p[1]) write_data(d, &p[2], p[1]);
    }
    write_cmd(d, CMD_COLMOD);  write_data(d, (uint8_t[]){ 0x55 }, 1);   /* 16 bpp */
    write_cmd(d, CMD_MADCTL);  write_data(d, &d->madctl, 1);
    write_cmd(d, d->invert ? CMD_INVON : CMD_INVOFF);
    write_cmd(d, CMD_NORON);   vTaskDelay(pdMS_TO_TICKS(10));
    write_cmd(d, CMD_DISPON);  vTaskDelay(pdMS_TO_TICKS(10));
}

/* SLEEP is SLPIN, blanked first with DISPOFF so no partial frame shows; OFF cuts
 * the panel rail. No rail callback -> OFF collapses to SLEEP. */
static esp_err_t set_power(bsp_display_t *self, bsp_display_power_t state) {
    ili9342c_t *d = (ili9342c_t *)self;
    if (!d->set_panel_power && state == BSP_DISPLAY_POWER_OFF)
        state = BSP_DISPLAY_POWER_SLEEP;
    if (state == d->power) return ESP_OK;

    if (d->power == BSP_DISPLAY_POWER_OFF && state != BSP_DISPLAY_POWER_OFF) {
        d->set_panel_power(d->panel_power_ctx, true);
        panel_init(d);
        d->power = BSP_DISPLAY_POWER_ON;
        if (state == BSP_DISPLAY_POWER_ON) return ESP_OK;
    }

    switch (state) {
    case BSP_DISPLAY_POWER_ON:
        write_cmd(d, CMD_SLPOUT);   vTaskDelay(pdMS_TO_TICKS(120));
        write_cmd(d, CMD_DISPON);   vTaskDelay(pdMS_TO_TICKS(10));
        break;
    case BSP_DISPLAY_POWER_SLEEP:
        write_cmd(d, CMD_DISPOFF);
        write_cmd(d, CMD_SLPIN);    vTaskDelay(pdMS_TO_TICKS(5));
        break;
    case BSP_DISPLAY_POWER_OFF:
        d->set_panel_power(d->panel_power_ctx, false);
        break;
    }
    d->power = state;
    return ESP_OK;
}

esp_err_t ili9342c_create(const ili9342c_config_t *cfg, bsp_display_t **out_display) {
    if (!cfg || !out_display || !cfg->width || !cfg->height) return ESP_ERR_INVALID_ARG;

    ili9342c_t *d = calloc(1, sizeof(*d));
    if (!d) return ESP_ERR_NO_MEM;
    d->dc_io           = cfg->dc_io;
    d->madctl          = cfg->madctl;
    d->invert          = cfg->invert;
    d->power           = BSP_DISPLAY_POWER_ON;
    d->set_reset       = cfg->set_reset;
    d->reset_ctx       = cfg->reset_ctx;
    d->set_backlight   = cfg->set_backlight;
    d->backlight_ctx   = cfg->backlight_ctx;
    d->set_panel_power = cfg->set_panel_power;
    d->panel_power_ctx = cfg->panel_power_ctx;

    const gpio_config_t out = {
        .pin_bit_mask = 1ULL << cfg->dc_io,
        .mode         = GPIO_MODE_OUTPUT,
    };
    gpio_config(&out);

    for (int i = 0; i < NBUF; i++) {
        d->dma_buf[i] = heap_caps_malloc(ILI9342C_DMA_CHUNK_BYTES, MALLOC_CAP_DMA);
        if (!d->dma_buf[i]) { deinit(&d->base); return ESP_ERR_NO_MEM; }
    }

    /* Write-only panel (no MISO): half-duplex lifts the ~26.6 MHz full-duplex
     * ceiling on GPIO-matrix pins, so the default 40 MHz is accepted. */
    const spi_device_interface_config_t dev_cfg = {
        .clock_speed_hz = cfg->clock_hz ? cfg->clock_hz : ILI9342C_SPI_DEFAULT_HZ,
        .mode           = 0,
        .spics_io_num   = cfg->cs_io,
        .queue_size     = NBUF,
        .flags          = SPI_DEVICE_HALFDUPLEX,
    };
    esp_err_t err = spi_bus_add_device(cfg->spi_host, &dev_cfg, &d->spi);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_add_device: %s", esp_err_to_name(err));
        d->spi = NULL;
        deinit(&d->base);
        return err;
    }

    panel_init(d);

    d->base.type   = BSP_DISPLAY_TYPE_SPI;
    d->base.size   = (bsp_size_t){ cfg->width, cfg->height };
    d->base.format = BSP_PIXEL_FORMAT_RGB565;
    d->base.draw_bitmap = draw_bitmap;
    d->base.deinit      = deinit;
    d->base.set_power   = set_power;
    if (d->set_backlight) d->base.set_brightness = set_brightness;

    ESP_LOGI(TAG, "ILI9342C %ux%u ready", cfg->width, cfg->height);
    *out_display = &d->base;
    return ESP_OK;
}
