/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ADC-ladder button provider using adc_continuous (DMA). N momentary buttons
 * share one ADC channel through a resistor divider; each button pulls a known
 * voltage when pressed. Returns a bsp_button_t vtable that the common layer
 * (src/bsp_button.c) drives -- register it with bsp_button_set_active().
 */

#pragma once
#include "bsp_button.h"
#include "esp_adc/adc_continuous.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef esp_err_t (*adc_button_raw_to_mv_fn)(int raw, int *out_mv, void *ctx);

typedef struct {
    adc_channel_t adc_channel;
    uint32_t      sample_freq_hz;         /* 0 -> 1000 Hz */

    adc_button_raw_to_mv_fn raw_to_mv;
    void                   *raw_to_mv_ctx;

    const uint16_t *centers_mv;   /* voltage @ press for button 0..count-1 (ascending) */
    uint16_t        idle_mv;
    uint8_t         count;
} adc_button_config_t;

/* Caller must release any oneshot handle on the same ADC unit first. */
esp_err_t adc_button_create(const adc_button_config_t *cfg, bsp_button_t **out_button);

#ifdef __cplusplus
}
#endif
