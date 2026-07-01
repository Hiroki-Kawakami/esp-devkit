/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ws2812.h"
#include "driver/rmt_tx.h"
#include "driver/rmt_encoder.h"
#include "esp_check.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include <stdlib.h>
#include <string.h>

static const char *TAG = "ws2812";

#define WS2812_DEFAULT_RESOLUTION_HZ   (10 * 1000 * 1000)  /* 100 ns/tick */
#define WS2812_DEFAULT_MEM_BLOCK       64
#define WS2812_TRANS_QUEUE_DEPTH       4
/* Timing at 10MHz (adjust with tick_ns() when resolution differs).
 * Values match the WS2812/WS2812B datasheet (T0H=0.35us, T0L=0.9us,
 * T1H=0.8us, T1L=0.45us; period = 1.25us). */
#define WS2812_T0H_NS   300
#define WS2812_T0L_NS   900
#define WS2812_T1H_NS   800
#define WS2812_T1L_NS   400

typedef struct {
    bsp_led_t              base;      /* must be first */
    rmt_channel_handle_t   chan;
    rmt_encoder_handle_t   encoder;
    uint16_t               count;
    uint8_t               *buf;        /* count * 3 bytes, GRB order */
} ws2812_dev_t;

static uint32_t ns_to_ticks(uint32_t ns, uint32_t resolution_hz) {
    uint64_t ticks = ((uint64_t)ns * resolution_hz + 999999999ULL) / 1000000000ULL;
    if (ticks == 0) ticks = 1;
    if (ticks > 0x7FFF) ticks = 0x7FFF;
    return (uint32_t)ticks;
}

static esp_err_t transmit_and_wait(ws2812_dev_t *dev) {
    const rmt_transmit_config_t tx_cfg = {
        .loop_count = 0,
        .flags      = { .eot_level = 0 },   /* line stays low = latches WS2812 */
    };
    esp_err_t err = rmt_transmit(dev->chan, dev->encoder, dev->buf,
                                 (size_t)dev->count * 3, &tx_cfg);
    if (err != ESP_OK) return err;
    return rmt_tx_wait_all_done(dev->chan, portMAX_DELAY);
}

static esp_err_t ws2812_set_rgb(bsp_led_t *self, uint8_t index, uint8_t r, uint8_t g, uint8_t b) {
    ws2812_dev_t *dev = (ws2812_dev_t *)self;
    uint8_t *p = &dev->buf[(size_t)index * 3];
    p[0] = g;
    p[1] = r;
    p[2] = b;
    return transmit_and_wait(dev);
}

static esp_err_t ws2812_clear(bsp_led_t *self) {
    ws2812_dev_t *dev = (ws2812_dev_t *)self;
    memset(dev->buf, 0, (size_t)dev->count * 3);
    return transmit_and_wait(dev);
}

static esp_err_t ws2812_deinit(bsp_led_t *self) {
    ws2812_dev_t *dev = (ws2812_dev_t *)self;
    if (!dev) return ESP_OK;
    if (dev->chan)    { rmt_disable(dev->chan); rmt_del_channel(dev->chan); }
    if (dev->encoder) rmt_del_encoder(dev->encoder);
    free(dev->buf);
    free(dev);
    return ESP_OK;
}

esp_err_t ws2812_create(const ws2812_config_t *config, bsp_led_t **out_led) {
    ESP_RETURN_ON_FALSE(config && out_led, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(config->count > 0, ESP_ERR_INVALID_ARG, TAG, "count must be > 0");
    ESP_RETURN_ON_FALSE(config->gpio != GPIO_NUM_NC, ESP_ERR_INVALID_ARG, TAG, "gpio required");

    ws2812_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->count = config->count;
    dev->buf   = calloc((size_t)config->count * 3, sizeof(uint8_t));
    if (!dev->buf) { free(dev); return ESP_ERR_NO_MEM; }

    dev->base.count   = (uint8_t)(config->count > 255 ? 255 : config->count);
    dev->base.set_rgb = ws2812_set_rgb;
    dev->base.clear   = ws2812_clear;
    dev->base.deinit  = ws2812_deinit;

    uint32_t resolution = config->resolution_hz ? config->resolution_hz : WS2812_DEFAULT_RESOLUTION_HZ;

    const rmt_tx_channel_config_t chan_cfg = {
        .gpio_num          = config->gpio,
        .clk_src           = RMT_CLK_SRC_DEFAULT,
        .resolution_hz     = resolution,
        .mem_block_symbols = config->mem_block_symbols ? config->mem_block_symbols : WS2812_DEFAULT_MEM_BLOCK,
        .trans_queue_depth = WS2812_TRANS_QUEUE_DEPTH,
        .flags             = { .invert_out = 0, .with_dma = 0 },
    };
    esp_err_t err = rmt_new_tx_channel(&chan_cfg, &dev->chan);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "rmt_new_tx_channel: %s", esp_err_to_name(err));
        goto fail;
    }

    const rmt_bytes_encoder_config_t enc_cfg = {
        .bit0 = { .level0 = 1, .duration0 = ns_to_ticks(WS2812_T0H_NS, resolution),
                  .level1 = 0, .duration1 = ns_to_ticks(WS2812_T0L_NS, resolution) },
        .bit1 = { .level0 = 1, .duration0 = ns_to_ticks(WS2812_T1H_NS, resolution),
                  .level1 = 0, .duration1 = ns_to_ticks(WS2812_T1L_NS, resolution) },
        .flags = { .msb_first = 1 },
    };
    err = rmt_new_bytes_encoder(&enc_cfg, &dev->encoder);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "rmt_new_bytes_encoder: %s", esp_err_to_name(err));
        goto fail;
    }

    err = rmt_enable(dev->chan);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "rmt_enable: %s", esp_err_to_name(err));
        goto fail;
    }

    /* Before `rmt_enable` runs, the line may float / carry POR noise, and the
     * chip's shift register can hold garbage that will show as random color
     * (often green given GRB order). The chip needs >50us of continuous low
     * to latch a reset; RMT holds the line at init_level=0 after enable, so
     * a millisecond of wait gets us past that threshold. Then we clear. */
    vTaskDelay(pdMS_TO_TICKS(2));
    (void)transmit_and_wait(dev);
    vTaskDelay(pdMS_TO_TICKS(2));
    (void)transmit_and_wait(dev);

    ESP_LOGI(TAG, "%u LED(s) on GPIO%d, RMT %lu Hz",
             (unsigned)config->count, (int)config->gpio, (unsigned long)resolution);
    *out_led = &dev->base;
    return ESP_OK;

fail:
    ws2812_deinit(&dev->base);
    return err;
}
