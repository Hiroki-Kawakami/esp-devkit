/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Core audio bring-up: builds the right bsp_audio provider for the
 * detected board (Core2 vs Basic) and registers it. Called once from bsp_init.
 */

#pragma once
#include "bsp.h"
#include "axp192.h"

esp_err_t core_audio_init(const bsp_config_t *config, axp192_handle_t axp);
