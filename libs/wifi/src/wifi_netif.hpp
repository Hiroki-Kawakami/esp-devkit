/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "esp_netif.h"

namespace wifi {

esp_netif_t* create_station_netif();

}  // namespace wifi
