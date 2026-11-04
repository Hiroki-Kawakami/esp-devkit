/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi_native.h"

#include <string.h>

void devkit_net_wifi_init_default(wifi_init_config_t *config)
{
    *config = (wifi_init_config_t)WIFI_INIT_CONFIG_DEFAULT();
}

esp_err_t devkit_net_wifi_set_sta(const uint8_t *ssid, size_t ssid_len, const char *password, const uint8_t *bssid,
                                  uint8_t channel)
{
    wifi_config_t config = {0};
    wifi_sta_config_t *sta = &config.sta;
    memcpy(sta->ssid, ssid, ssid_len < sizeof(sta->ssid) ? ssid_len : sizeof(sta->ssid));
    if (password) {
        strncpy((char *)sta->password, password, sizeof(sta->password) - 1);
        sta->threshold.authmode = WIFI_AUTH_WPA2_PSK;
    }
    if (bssid) {
        sta->bssid_set = true;
        memcpy(sta->bssid, bssid, sizeof(sta->bssid));
    }
    sta->channel = channel;
    sta->scan_method = WIFI_ALL_CHANNEL_SCAN;
    sta->pmf_cfg.capable = true;
    sta->sae_pwe_h2e = WPA3_SAE_PWE_BOTH;
    return esp_wifi_set_config(WIFI_IF_STA, &config);
}
