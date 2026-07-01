/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * WS2812 addressable RGB LED driver (RMT, in-tree — no managed_components
 * `led_strip` dependency). Produces a bsp_led_t provider for the common LED
 * layer (src/bsp_led.c); the board registers it with bsp_led_set_active().
 *
 * The driver owns one RMT TX channel plus a bytes-encoder configured with the
 * WS2812 T0H/T0L/T1H/T1L symbols; each set_rgb/clear re-transmits the whole
 * strip and blocks until the transaction completes. GRB byte order (standard
 * WS2812) is baked in.
 */

#pragma once
#include "bsp_led.h"
#include "driver/gpio.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    gpio_num_t gpio;
    uint16_t   count;              /*!< LEDs in the strip (>= 1)                      */
    uint32_t   resolution_hz;      /*!< RMT counter resolution; 0 -> 10 MHz (100 ns) */
    size_t     mem_block_symbols;  /*!< RMT symbol block size; 0 -> 64                */
} ws2812_config_t;

esp_err_t ws2812_create(const ws2812_config_t *config, bsp_led_t **out_led);

#ifdef __cplusplus
}
#endif
