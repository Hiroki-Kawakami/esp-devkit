/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GDEY0154D67 (SSD1681) SPI e-paper driver. See gdey0154d67.h for the design
 * notes.
 */

#include "gdey0154d67.h"

#include <stdlib.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "gdey0154d67";

#define BUSY_TIMEOUT_MS   10000
#define SPI_CHUNK_BYTES   4000

typedef struct gdey0154d67_dev gdey0154d67_dev_t;
struct gdey0154d67_dev {
    gdey0154d67_config_t cfg;
    spi_device_handle_t  spi;
    uint16_t             width;
    uint16_t             height;
};

static void cs(gdey0154d67_dev_t *d, int level) { gpio_set_level(d->cfg.cs_io, level); }
static void dc(gdey0154d67_dev_t *d, int level) { gpio_set_level(d->cfg.dc_io, level); }

static void write_bytes(gdey0154d67_dev_t *d, const uint8_t *data, size_t len) {
    for (size_t off = 0; off < len; off += SPI_CHUNK_BYTES) {
        size_t chunk = len - off < SPI_CHUNK_BYTES ? len - off : SPI_CHUNK_BYTES;
        spi_transaction_t t = { .length = chunk * 8, .tx_buffer = data + off };
        spi_device_polling_transmit(d->spi, &t);
    }
}

static void cmd(gdey0154d67_dev_t *d, uint8_t c) {
    cs(d, 0); dc(d, 0);
    write_bytes(d, &c, 1);
    cs(d, 1);
}

static void data1(gdey0154d67_dev_t *d, uint8_t v) {
    cs(d, 0); dc(d, 1);
    write_bytes(d, &v, 1);
    cs(d, 1);
}

static void data_bulk(gdey0154d67_dev_t *d, const uint8_t *data, size_t len) {
    cs(d, 0); dc(d, 1);
    write_bytes(d, data, len);
    cs(d, 1);
}

static esp_err_t wait_busy(gdey0154d67_dev_t *d, uint32_t timeout_ms) {
    if (d->cfg.busy_io == GPIO_NUM_NC) { vTaskDelay(pdMS_TO_TICKS(50)); return ESP_OK; }
    uint32_t waited = 0;
    while (gpio_get_level(d->cfg.busy_io)) {
        vTaskDelay(pdMS_TO_TICKS(5));
        waited += 5;
        if (timeout_ms && waited >= timeout_ms) {
            ESP_LOGW(TAG, "busy timeout");
            return ESP_ERR_TIMEOUT;
        }
    }
    return ESP_OK;
}

static void hw_reset(gdey0154d67_dev_t *d) {
    if (d->cfg.reset_io == GPIO_NUM_NC) return;
    gpio_set_level(d->cfg.reset_io, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(d->cfg.reset_io, 0);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(d->cfg.reset_io, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
}

/* GoodDisplay vendor full-refresh waveform (fewer flashes than the SSD1681 OTP
 * waveform). Bytes 0..152 are the LUT; 153..158 the paired EOPT/gate/source/VCOM
 * register values -- see load_full_lut(). */
static const uint8_t LUT_FULL[159] = {
    0x80, 0x48, 0x40, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x40, 0x48, 0x80, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x80, 0x48, 0x40, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x40, 0x48, 0x80, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0xA, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x8, 0x1, 0x0, 0x8, 0x1, 0x0, 0x2,
    0xA, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
    0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x0, 0x0, 0x0,
    0x22, 0x17, 0x41, 0x0, 0x32, 0x20,
};

static void load_full_lut(gdey0154d67_dev_t *d) {
    cmd(d, 0x32);
    data_bulk(d, LUT_FULL, 153);
    wait_busy(d, BUSY_TIMEOUT_MS);

    cmd(d, 0x3F);                       /* end option (EOPT) */
    data1(d, LUT_FULL[153]);
    cmd(d, 0x03);                       /* gate driving voltage */
    data1(d, LUT_FULL[154]);
    cmd(d, 0x04);                       /* source driving voltage */
    data1(d, LUT_FULL[155]);
    data1(d, LUT_FULL[156]);
    data1(d, LUT_FULL[157]);
    cmd(d, 0x2C);                       /* VCOM */
    data1(d, LUT_FULL[158]);
}

/* Data entry 0x01 (X increment, Y decrement) + cursor at the last gate line
 * renders a top-to-bottom, MSB-left frame buffer upright (the layout the
 * provider packs). Ends by uploading the full LUT. */
static esp_err_t panel_init(gdey0154d67_dev_t *d) {
    const uint16_t w = d->width, h = d->height;

    hw_reset(d);
    vTaskDelay(pdMS_TO_TICKS(50));

    esp_err_t err = wait_busy(d, BUSY_TIMEOUT_MS);
    if (err != ESP_OK) return err;
    cmd(d, 0x12);                       /* SWRESET */
    err = wait_busy(d, BUSY_TIMEOUT_MS);
    if (err != ESP_OK) return err;

    cmd(d, 0x01);                       /* Driver output control */
    data1(d, (h - 1) & 0xFF);
    data1(d, ((h - 1) >> 8) & 0xFF);
    data1(d, 0x01);                     /* GD=1: gate scan G0->Gn (panel orientation) */

    cmd(d, 0x11);                       /* Data entry mode */
    data1(d, 0x01);

    cmd(d, 0x44);                       /* RAM X window (byte units) */
    data1(d, 0x00);
    data1(d, (w - 1) / 8);

    cmd(d, 0x45);                       /* RAM Y window */
    data1(d, (h - 1) & 0xFF);
    data1(d, ((h - 1) >> 8) & 0xFF);
    data1(d, 0x00);
    data1(d, 0x00);

    cmd(d, 0x3C);                       /* Border waveform */
    data1(d, 0x01);

    cmd(d, 0x18);                       /* Temperature sensor: internal */
    data1(d, 0x80);

    cmd(d, 0x22);                       /* load temperature into the waveform */
    data1(d, 0xB1);
    cmd(d, 0x20);

    cmd(d, 0x4E);                       /* RAM X cursor */
    data1(d, 0x00);
    cmd(d, 0x4F);                       /* RAM Y cursor */
    data1(d, (h - 1) & 0xFF);
    data1(d, ((h - 1) >> 8) & 0xFF);

    err = wait_busy(d, BUSY_TIMEOUT_MS);
    if (err != ESP_OK) return err;

    load_full_lut(d);
    return ESP_OK;
}

esp_err_t gdey0154d67_reset(gdey0154d67_handle_t h) {
    if (!h) return ESP_ERR_INVALID_ARG;
    return panel_init(h);
}

esp_err_t gdey0154d67_update_full(gdey0154d67_handle_t d, const uint8_t *packed) {
    if (!d || !packed) return ESP_ERR_INVALID_ARG;
    const size_t len = (size_t)(d->width / 8) * d->height;

    /* The vendor full LUT drives on the old(0x26) vs new(0x24) pair; for a
     * full redraw both hold the target image (a clean flash to the new frame). */
    cmd(d, 0x24);                       /* new image RAM */
    data_bulk(d, packed, len);
    cmd(d, 0x26);                       /* base image RAM */
    data_bulk(d, packed, len);

    cmd(d, 0x22);                       /* display update control 2 */
    data1(d, 0xC7);                     /* clk+analog, display (uploaded LUT, no OTP) */
    cmd(d, 0x20);                       /* master activation */
    return wait_busy(d, BUSY_TIMEOUT_MS);
}

esp_err_t gdey0154d67_sleep(gdey0154d67_handle_t d) {
    if (!d) return ESP_ERR_INVALID_ARG;
    cmd(d, 0x10);                       /* deep sleep mode */
    data1(d, 0x01);
    vTaskDelay(pdMS_TO_TICKS(100));
    return ESP_OK;
}

esp_err_t gdey0154d67_wait_idle(gdey0154d67_handle_t d, uint32_t timeout_ms) {
    if (!d) return ESP_ERR_INVALID_ARG;
    return wait_busy(d, timeout_ms);
}

esp_err_t gdey0154d67_create(const gdey0154d67_config_t *cfg, gdey0154d67_handle_t *out_handle) {
    if (!cfg || !out_handle) return ESP_ERR_INVALID_ARG;
    if (cfg->cs_io == GPIO_NUM_NC || cfg->dc_io == GPIO_NUM_NC) return ESP_ERR_INVALID_ARG;

    gdey0154d67_dev_t *d = calloc(1, sizeof(*d));
    if (!d) return ESP_ERR_NO_MEM;
    d->cfg    = *cfg;
    d->width  = cfg->width  ? cfg->width  : GDEY0154D67_DEFAULT_WIDTH;
    d->height = cfg->height ? cfg->height : GDEY0154D67_DEFAULT_HEIGHT;

    uint64_t out_mask = (1ULL << cfg->cs_io) | (1ULL << cfg->dc_io);
    if (cfg->reset_io != GPIO_NUM_NC) out_mask |= (1ULL << cfg->reset_io);
    gpio_config_t out = {
        .pin_bit_mask = out_mask,
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    gpio_config(&out);
    gpio_set_level(cfg->cs_io, 1);
    gpio_set_level(cfg->dc_io, 1);
    if (cfg->reset_io != GPIO_NUM_NC) gpio_set_level(cfg->reset_io, 1);

    if (cfg->busy_io != GPIO_NUM_NC) {
        gpio_config_t in = {
            .pin_bit_mask = 1ULL << cfg->busy_io,
            .mode         = GPIO_MODE_INPUT,
            .pull_up_en   = GPIO_PULLUP_DISABLE,
            .pull_down_en = GPIO_PULLDOWN_DISABLE,
            .intr_type    = GPIO_INTR_DISABLE,
        };
        gpio_config(&in);
    }

    const spi_device_interface_config_t dev_cfg = {
        .clock_speed_hz = cfg->clock_hz ? cfg->clock_hz : GDEY0154D67_SPI_DEFAULT_HZ,
        .mode           = GDEY0154D67_SPI_MODE,
        .spics_io_num   = -1,               /* CS driven manually */
        .queue_size     = 1,
    };
    esp_err_t err = spi_bus_add_device(cfg->spi_host, &dev_cfg, &d->spi);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_add_device: %s", esp_err_to_name(err));
        free(d);
        return err;
    }

    err = panel_init(d);
    if (err != ESP_OK) {
        spi_bus_remove_device(d->spi);
        free(d);
        return err;
    }

    ESP_LOGI(TAG, "GDEY0154D67 %ux%u ready", d->width, d->height);
    *out_handle = d;
    return ESP_OK;
}

esp_err_t gdey0154d67_destroy(gdey0154d67_handle_t d) {
    if (!d) return ESP_ERR_INVALID_ARG;
    if (d->spi) spi_bus_remove_device(d->spi);
    free(d);
    return ESP_OK;
}
