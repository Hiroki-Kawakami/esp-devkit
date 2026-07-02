/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "adc_button.h"
#include <stdlib.h>
#include <string.h>
#include "esp_check.h"
#include "esp_log.h"
#include "soc/soc_caps.h"

static const char *TAG = "adc_button";

#define ADC_BUTTON_MAX_COUNT       8
#define DEFAULT_SAMPLE_FREQ_HZ     1000
#define CONV_FRAME_BYTES           (4 * SOC_ADC_DIGI_RESULT_BYTES)
#define MAX_STORE_BUF_BYTES        (8 * CONV_FRAME_BYTES)
#define READ_BATCH_SAMPLES         32

typedef struct {
    uint16_t min_mv;
    uint16_t max_mv;
} window_t;

struct adc_button_dev {
    bsp_button_t             base;
    adc_continuous_handle_t  handle;
    adc_button_raw_to_mv_fn  raw_to_mv;
    void                    *raw_to_mv_ctx;
    window_t                 windows[ADC_BUTTON_MAX_COUNT];
    bool                     last_pressed[ADC_BUTTON_MAX_COUNT];
};

static inline uint16_t midpoint(uint16_t a, uint16_t b) { return (uint16_t)((a + b) / 2); }

static esp_err_t adc_button_sample(bsp_button_t *self, bool *pressed, uint8_t max) {
    struct adc_button_dev *dev = (struct adc_button_dev *)self;
    if (max > dev->base.count) max = dev->base.count;

    adc_continuous_data_t parsed[READ_BATCH_SAMPLES];
    uint32_t num = 0;
    esp_err_t err = adc_continuous_read_parse(dev->handle, parsed,
                                              READ_BATCH_SAMPLES, &num, 0);
    if (err == ESP_ERR_TIMEOUT || num == 0) {
        memcpy(pressed, dev->last_pressed, max);
        return ESP_OK;
    }
    if (err != ESP_OK) return err;

    uint32_t raw = 0;
    bool have = false;
    for (int32_t i = (int32_t)num - 1; i >= 0; i--) {
        if (parsed[i].valid) { raw = parsed[i].raw_data; have = true; break; }
    }
    if (!have) {
        memcpy(pressed, dev->last_pressed, max);
        return ESP_OK;
    }

    int mv = 0;
    err = dev->raw_to_mv((int)raw, &mv, dev->raw_to_mv_ctx);
    if (err != ESP_OK) return err;

    memset(pressed, 0, max);
    for (uint8_t i = 0; i < max; i++) {
        if (mv >= dev->windows[i].min_mv && mv <= dev->windows[i].max_mv) {
            pressed[i] = true;
            break;
        }
    }
    memcpy(dev->last_pressed, pressed, max);
    return ESP_OK;
}

static esp_err_t adc_button_deinit(bsp_button_t *self) {
    struct adc_button_dev *dev = (struct adc_button_dev *)self;
    if (!dev) return ESP_OK;
    if (dev->handle) {
        adc_continuous_stop(dev->handle);
        adc_continuous_deinit(dev->handle);
    }
    free(dev);
    return ESP_OK;
}

esp_err_t adc_button_create(const adc_button_config_t *cfg, bsp_button_t **out_button) {
    ESP_RETURN_ON_FALSE(cfg && out_button && cfg->raw_to_mv && cfg->centers_mv,
                        ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(cfg->count > 0 && cfg->count <= ADC_BUTTON_MAX_COUNT,
                        ESP_ERR_INVALID_ARG, TAG, "bad count");

    struct adc_button_dev *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->base.count  = cfg->count;
    dev->base.sample = adc_button_sample;
    dev->base.deinit = adc_button_deinit;
    dev->raw_to_mv     = cfg->raw_to_mv;
    dev->raw_to_mv_ctx = cfg->raw_to_mv_ctx;

    /* Per-button voltage windows: midpoints between neighbours; the first
     * button's lower bound mirrors its upper distance from the center. */
    for (uint8_t i = 0; i < cfg->count; i++) {
        uint16_t c = cfg->centers_mv[i];
        uint16_t lower = (i == 0) ? 0 : midpoint(cfg->centers_mv[i - 1], c);
        uint16_t upper = (i == cfg->count - 1)
                              ? midpoint(c, cfg->idle_mv)
                              : midpoint(c, cfg->centers_mv[i + 1]);
        if (i == 0) {
            uint16_t margin = upper - c;
            lower = c > margin ? c - margin : 0;
        }
        dev->windows[i].min_mv = lower;
        dev->windows[i].max_mv = upper;
        ESP_LOGD(TAG, "btn %u: %u..%u mV (center %u)", i, lower, upper, c);
    }

    const adc_continuous_handle_cfg_t hcfg = {
        .max_store_buf_size = MAX_STORE_BUF_BYTES,
        .conv_frame_size    = CONV_FRAME_BYTES,
    };
    esp_err_t err = adc_continuous_new_handle(&hcfg, &dev->handle);
    if (err != ESP_OK) goto fail;

    adc_digi_pattern_config_t pattern = {
        .atten     = ADC_ATTEN_DB_0,
        .channel   = (uint8_t)cfg->adc_channel,
        .unit      = ADC_UNIT_1,
        .bit_width = SOC_ADC_DIGI_MAX_BITWIDTH,
    };
    const adc_continuous_config_t ccfg = {
        .pattern_num    = 1,
        .adc_pattern    = &pattern,
        .sample_freq_hz = cfg->sample_freq_hz ? cfg->sample_freq_hz : DEFAULT_SAMPLE_FREQ_HZ,
        .conv_mode      = ADC_CONV_SINGLE_UNIT_1,
        .format         = ADC_DIGI_OUTPUT_FORMAT_TYPE2,
    };
    err = adc_continuous_config(dev->handle, &ccfg);
    if (err != ESP_OK) goto fail;
    err = adc_continuous_start(dev->handle);
    if (err != ESP_OK) goto fail;

    *out_button = &dev->base;
    return ESP_OK;

fail:
    adc_button_deinit(&dev->base);
    return err;
}
