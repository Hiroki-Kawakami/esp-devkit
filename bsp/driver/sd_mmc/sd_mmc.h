/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stddef.h>
#include "bsp_sd.h"
#include "driver/sdmmc_host.h"

typedef esp_err_t (*sd_mmc_power_acquire_t)(void *context,
                                             sd_pwr_ctrl_handle_t *out_power);
typedef esp_err_t (*sd_mmc_power_release_t)(void *context,
                                             sd_pwr_ctrl_handle_t power);

typedef struct {
    sdmmc_host_t host;
    sdmmc_slot_config_t slot_config;
    size_t allocation_unit_size;
    sd_mmc_power_acquire_t power_acquire;
    sd_mmc_power_release_t power_release;
    void *power_context;
} sd_mmc_config_t;

esp_err_t sd_mmc_create(const sd_mmc_config_t *config, bsp_sd_t **out_sd);
