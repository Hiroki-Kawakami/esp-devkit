/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_power provider: a fake battery + external supply, steered by
 * bsp_harness_sim_power_inject. The board's simulator bring-up registers it
 * with bsp_power_set_active().
 */

#pragma once
#include "bsp_power.h"

typedef struct {
    uint32_t caps;      /*!< the device provider's BSP_POWER_CAP_* */
    uint32_t empty_mv;  /*!< 0% endpoint */
    uint32_t full_mv;   /*!< 100% endpoint */
} power_sim_config_t;

esp_err_t power_sim_create(const power_sim_config_t *config, bsp_power_t **out_power);
