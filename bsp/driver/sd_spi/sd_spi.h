/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "bsp_sd.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"

typedef enum {
    SD_SPI_BUS_BORROWED,
    SD_SPI_BUS_MANAGED,
} sd_spi_bus_lifecycle_t;

typedef struct {
    spi_host_device_t spi_host;
    gpio_num_t cs_io;
    sd_spi_bus_lifecycle_t bus_lifecycle;
    spi_bus_config_t bus_config;
    spi_dma_chan_t dma_channel;
} sd_spi_config_t;

esp_err_t sd_spi_create(const sd_spi_config_t *config, bsp_sd_t **out_sd);
