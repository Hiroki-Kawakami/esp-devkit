/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi_netif.hpp"

#include "esp_wifi_default.h"

esp_netif_t* wifi::create_station_netif() {
    return esp_netif_create_default_wifi_sta();
}
