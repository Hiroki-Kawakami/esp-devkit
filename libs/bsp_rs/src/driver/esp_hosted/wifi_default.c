/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi_default.h"

void bsp_rs_wifi_init_default(wifi_init_config_t *config)
{
    *config = (wifi_init_config_t)WIFI_INIT_CONFIG_DEFAULT();
}
