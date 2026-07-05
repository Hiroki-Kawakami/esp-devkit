/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_power provider: a fake battery + VBUS so the seam is
 * exercisable in the simulator. The board's simulator bring-up registers it
 * with bsp_power_set_active().
 */

#pragma once
#include "bsp_power.h"

esp_err_t power_sim_create(bsp_power_t **out_power);
