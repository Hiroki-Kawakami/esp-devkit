/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_wifi.h"

/* WIFI_INIT_CONFIG_DEFAULT(), which is a macro. */
void devkit_net_wifi_init_default(wifi_init_config_t *config);

/* Sets the station config in C: bindgen lays out wifi_sta_config_t's he_*
 * bitfields wrong. `password` and `bssid` may be NULL; channel 0 scans all. */
esp_err_t devkit_net_wifi_set_sta(const uint8_t *ssid, size_t ssid_len, const char *password, const uint8_t *bssid,
                                  uint8_t channel);

typedef struct {
    uint8_t ssid[32];
    uint8_t ssid_len;
    uint8_t bssid[6];
    uint8_t channel;
    int8_t rssi;
    uint8_t authmode;
} devkit_net_scan_record_t;

/* Scans all channels, blocking until done. `count` holds the room in
 * `records` and returns how many were filled. */
esp_err_t devkit_net_wifi_scan(devkit_net_scan_record_t *records, uint16_t *count);
