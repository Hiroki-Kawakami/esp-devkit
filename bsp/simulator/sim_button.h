/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host stand-in for a board's physical buttons: a level provider that always
 * samples "released", so the only way to press one is bsp_harness_button_inject.
 * Gives the simulator the same bsp_button id space as the device so scripted
 * button presses drive the same app callbacks on both.
 */

#pragma once
#include "bsp_button.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t sim_button_create(uint8_t count, bsp_button_raw_t **out_button);

#ifdef __cplusplus
}
#endif
