/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host counterpart of ESP-IDF v6.0.2 <esp_mac.h>. The simulator uses a
 * persistent, locally administered factory MAC instead of hardware eFuse.
 */

#pragma once

#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#ifndef MAC2STR
#define MAC2STR(a) (a)[0], (a)[1], (a)[2], (a)[3], (a)[4], (a)[5]
#define MACSTR "%02x:%02x:%02x:%02x:%02x:%02x"
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    ESP_MAC_WIFI_STA,
    ESP_MAC_WIFI_SOFTAP,
    ESP_MAC_BT,
    ESP_MAC_ETH,
    ESP_MAC_IEEE802154,
    ESP_MAC_BASE,
    ESP_MAC_EFUSE_FACTORY,
    ESP_MAC_EFUSE_CUSTOM,
    ESP_MAC_EFUSE_EXT,
} esp_mac_type_t;

#define ONE_UNIVERSAL_MAC_ADDR  1
#define TWO_UNIVERSAL_MAC_ADDR  2
#define FOUR_UNIVERSAL_MAC_ADDR 4
#define UNIVERSAL_MAC_ADDR_NUM  FOUR_UNIVERSAL_MAC_ADDR

esp_err_t esp_base_mac_addr_set(const uint8_t *mac);
esp_err_t esp_base_mac_addr_get(uint8_t *mac);
esp_err_t esp_efuse_mac_get_custom(uint8_t *mac);
esp_err_t esp_efuse_mac_get_default(uint8_t *mac);
esp_err_t esp_read_mac(uint8_t *mac, esp_mac_type_t type);
esp_err_t esp_derive_local_mac(uint8_t *local_mac, const uint8_t *universal_mac);
esp_err_t esp_iface_mac_addr_set(const uint8_t *mac, esp_mac_type_t type);
size_t esp_mac_addr_len_get(esp_mac_type_t type);

#ifdef __cplusplus
}
#endif
