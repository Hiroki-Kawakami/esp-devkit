/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GDEY0154D67 EPD display provider: wraps the gdey0154d67 SSD1681 driver into a
 * bsp_display_t. See gdey0154d67_epd.c for the GRAM/refresh model.
 */

#pragma once
#include "bsp_display.h"
#include "gdey0154d67.h"

typedef struct {
    spi_host_device_t spi_host;
    gpio_num_t        cs_io;
    gpio_num_t        dc_io;
    gpio_num_t        reset_io;
    gpio_num_t        busy_io;
    int               clock_hz;   /* 0 -> driver default */
} gdey0154d67_epd_config_t;

esp_err_t gdey0154d67_epd_create(const gdey0154d67_epd_config_t *cfg, bsp_display_t **out_display);
