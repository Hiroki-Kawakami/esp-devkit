/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "esp_wifi.h"

/* WIFI_INIT_CONFIG_DEFAULT(), which is a macro. */
void bsp_rs_wifi_init_default(wifi_init_config_t *config);
