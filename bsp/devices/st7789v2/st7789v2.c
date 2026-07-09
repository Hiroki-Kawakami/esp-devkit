/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "st7789v2.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "st7789v2";

#define CHUNK_PX  (ST7789V2_DMA_CHUNK_BYTES / 2)   /* RGB565 pixels per DMA chunk */
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
    gpio_num_t reset_io;
    uint16_t   x_offset, y_offset;
    uint8_t    madctl;
    bool       invert;
    bsp_display_power_t power;
    st7789v2_backlight_cb_t set_backlight;
    void      *backlight_ctx;
    st7789v2_power_cb_t set_panel_power;
    void      *panel_power_ctx;

    uint16_t *dma_buf[NBUF];
    spi_transaction_t trans[NBUF];
    uint16_t *rotate_buf;   /* lazily grown scratch for rotated blits */
    size_t    rotate_px;
} st7789v2_t;

static inline void dc(st7789v2_t *d, int level) { gpio_set_level(d->dc_io, level); }

static void tx(st7789v2_t *d, const uint8_t *data, size_t len) {
    spi_transaction_t t = { .length = len * 8, .tx_buffer = data };
    spi_device_polling_transmit(d->spi, &t);
}

static void write_cmd(st7789v2_t *d, uint8_t c) {
    dc(d, 0);
    tx(d, &c, 1);
}

static void write_data(st7789v2_t *d, const uint8_t *data, size_t len) {
    dc(d, 1);
    tx(d, data, len);
}

static void set_window(st7789v2_t *d, int x0, int y0, int x1, int y1) {
    x0 += d->x_offset; x1 += d->x_offset;
    y0 += d->y_offset; y1 += d->y_offset;
    uint8_t col[4] = { x0 >> 8, x0 & 0xFF, x1 >> 8, x1 & 0xFF };
    uint8_t row[4] = { y0 >> 8, y0 & 0xFF, y1 >> 8, y1 & 0xFF };
    write_cmd(d, CMD_CASET); write_data(d, col, 4);
    write_cmd(d, CMD_RASET); write_data(d, row, 4);
    write_cmd(d, CMD_RAMWR);
}

/* Stream `count` RGB565 pixels to GRAM, byte-swapping to the panel's big-endian
 * order into DMA bounce buffers and overlapping the swap of the next chunk with
 * the DMA of the current one. */
static void stream_pixels(st7789v2_t *d, const uint16_t *px, size_t count) {
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
    st7789v2_t *d = (st7789v2_t *)self;
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
    st7789v2_t *d = (st7789v2_t *)self;
    d->set_backlight(d->backlight_ctx, brightness);
    return ESP_OK;
}

static esp_err_t deinit(bsp_display_t *self) {
    st7789v2_t *d = (st7789v2_t *)self;
    if (d->spi) spi_bus_remove_device(d->spi);
    for (int i = 0; i < NBUF; i++) heap_caps_free(d->dma_buf[i]);
    free(d->rotate_buf);
    free(d);
    return ESP_OK;
}

static void hw_reset(st7789v2_t *d) {
    if (d->reset_io == GPIO_NUM_NC) return;
    gpio_set_level(d->reset_io, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(d->reset_io, 0);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(d->reset_io, 1);
    vTaskDelay(pdMS_TO_TICKS(120));
}

static void panel_init(st7789v2_t *d) {
    hw_reset(d);
    write_cmd(d, CMD_SWRESET);           vTaskDelay(pdMS_TO_TICKS(120));
    write_cmd(d, CMD_SLPOUT);            vTaskDelay(pdMS_TO_TICKS(120));
    write_cmd(d, CMD_COLMOD);            write_data(d, (uint8_t[]){ 0x55 }, 1);  /* 16 bpp */
    write_cmd(d, CMD_MADCTL);            write_data(d, &d->madctl, 1);
    write_cmd(d, d->invert ? CMD_INVON : CMD_INVOFF);
    write_cmd(d, CMD_NORON);             vTaskDelay(pdMS_TO_TICKS(10));
    write_cmd(d, CMD_DISPON);            vTaskDelay(pdMS_TO_TICKS(10));
}

/* SLEEP is SLPIN, blanked first with DISPOFF so no partial frame shows; OFF cuts
 * the panel rail. No rail callback -> OFF collapses to SLEEP. */
static esp_err_t set_power(bsp_display_t *self, bsp_display_power_t state) {
    st7789v2_t *d = (st7789v2_t *)self;
    if (!d->set_panel_power && state == BSP_DISPLAY_POWER_OFF)
        state = BSP_DISPLAY_POWER_SLEEP;
    if (state == d->power) return ESP_OK;

    /* Rail was cut: re-power and re-init before any command. */
    if (d->power == BSP_DISPLAY_POWER_OFF && state != BSP_DISPLAY_POWER_OFF) {
        d->set_panel_power(d->panel_power_ctx, true);
        panel_init(d);
        d->power = BSP_DISPLAY_POWER_ON;
        if (state == BSP_DISPLAY_POWER_ON) return ESP_OK;
    }

    switch (state) {
    case BSP_DISPLAY_POWER_ON:   /* from SLEEP */
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

esp_err_t st7789v2_create(const st7789v2_config_t *cfg, bsp_display_t **out_display) {
    if (!cfg || !out_display || !cfg->width || !cfg->height) return ESP_ERR_INVALID_ARG;

    st7789v2_t *d = calloc(1, sizeof(*d));
    if (!d) return ESP_ERR_NO_MEM;
    d->dc_io         = cfg->dc_io;
    d->reset_io      = cfg->reset_io;
    d->x_offset      = cfg->x_offset;
    d->y_offset      = cfg->y_offset;
    d->madctl        = cfg->madctl;
    d->invert        = cfg->invert;
    d->power         = BSP_DISPLAY_POWER_ON;
    d->set_backlight = cfg->set_backlight;
    d->backlight_ctx = cfg->backlight_ctx;
    d->set_panel_power = cfg->set_panel_power;
    d->panel_power_ctx = cfg->panel_power_ctx;

    const gpio_config_t out = {
        .pin_bit_mask = (1ULL << cfg->dc_io) |
                        (cfg->reset_io != GPIO_NUM_NC ? 1ULL << cfg->reset_io : 0),
        .mode         = GPIO_MODE_OUTPUT,
    };
    gpio_config(&out);

    for (int i = 0; i < NBUF; i++) {
        d->dma_buf[i] = heap_caps_malloc(ST7789V2_DMA_CHUNK_BYTES, MALLOC_CAP_DMA);
        if (!d->dma_buf[i]) { deinit(&d->base); return ESP_ERR_NO_MEM; }
    }

    /* Write-only panel (no MISO): half-duplex lifts the ~26.6 MHz full-duplex
     * ceiling on GPIO-matrix pins, so the default 40 MHz is accepted. */
    const spi_device_interface_config_t dev_cfg = {
        .clock_speed_hz = cfg->clock_hz ? cfg->clock_hz : ST7789V2_SPI_DEFAULT_HZ,
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
    d->base.set_power   = set_power;   /* SLEEP always works; OFF needs a rail cb */
    if (d->set_backlight) d->base.set_brightness = set_brightness;

    ESP_LOGI(TAG, "ST7789V2 %ux%u ready", cfg->width, cfg->height);
    *out_display = &d->base;
    return ESP_OK;
}
