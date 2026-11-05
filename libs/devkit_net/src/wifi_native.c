/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi_native.h"

#include <stdlib.h>
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
        size_t len = strlen(password);
        memcpy(sta->password, password, len < sizeof(sta->password) ? len : sizeof(sta->password));
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

esp_err_t devkit_net_wifi_scan(devkit_net_scan_record_t *records, uint16_t *count)
{
    esp_err_t err = esp_wifi_scan_start(NULL, true);
    if (err != ESP_OK) {
        *count = 0;
        return err;
    }
    uint16_t n = *count;
    wifi_ap_record_t *aps = calloc(n ? n : 1, sizeof(*aps));
    if (!aps) {
        esp_wifi_clear_ap_list();
        *count = 0;
        return ESP_ERR_NO_MEM;
    }
    err = esp_wifi_scan_get_ap_records(&n, aps);
    if (err != ESP_OK) {
        n = 0;
    }
    for (uint16_t i = 0; i < n; i++) {
        devkit_net_scan_record_t *r = &records[i];
        size_t len = strnlen((const char *)aps[i].ssid, sizeof(r->ssid));
        memcpy(r->ssid, aps[i].ssid, len);
        r->ssid_len = (uint8_t)len;
        memcpy(r->bssid, aps[i].bssid, sizeof(r->bssid));
        r->channel = aps[i].primary;
        r->rssi = aps[i].rssi;
        r->authmode = (uint8_t)aps[i].authmode;
    }
    free(aps);
    *count = n;
    return err;
}
