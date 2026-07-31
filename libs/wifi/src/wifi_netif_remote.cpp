/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi_netif.hpp"

#include <cassert>

#include "esp_event.h"
#include "esp_log.h"
#include "esp_private/wifi.h"
#include "esp_wifi.h"
#include "esp_wifi_default.h"
#include "esp_wifi_netif.h"

namespace wifi {
namespace {

static const char* TAG = "wifi.netif.remote";

struct StationNetifState {
    esp_netif_t* netif = nullptr;
    bool started = false;
};

StationNetifState station;

void station_start(void* argument, esp_event_base_t event_base,
                   int32_t event_id, void* event_data) {
    auto* state = static_cast<StationNetifState*>(argument);
    if (state->started || esp_netif_is_netif_up(state->netif)) {
        ESP_LOGW(TAG, "ignore duplicate Wi-Fi STA start event");
        return;
    }

    auto driver = static_cast<wifi_netif_driver_t>(
        esp_netif_get_io_driver(state->netif));
    uint8_t address[6];
    esp_err_t error = esp_wifi_get_if_mac(driver, address);
    if (error != ESP_OK) {
        ESP_LOGE(TAG, "esp_wifi_get_if_mac failed: %s",
                 esp_err_to_name(error));
        return;
    }
    if (esp_wifi_is_if_ready_when_started(driver)) {
        error = esp_wifi_register_if_rxcb(driver, esp_netif_receive,
                                          state->netif);
        if (error != ESP_OK) {
            ESP_LOGE(TAG, "esp_wifi_register_if_rxcb failed: %s",
                     esp_err_to_name(error));
            return;
        }
    }
    error = esp_wifi_internal_reg_netstack_buf_cb(
        esp_netif_netstack_buf_ref, esp_netif_netstack_buf_free);
    if (error != ESP_OK) {
        ESP_LOGE(TAG, "netstack callback registration failed: %s",
                 esp_err_to_name(error));
        return;
    }
    error = esp_netif_set_mac(state->netif, address);
    if (error != ESP_OK) {
        ESP_LOGE(TAG, "esp_netif_set_mac failed: %s",
                 esp_err_to_name(error));
        return;
    }
    esp_netif_action_start(state->netif, event_base, event_id, event_data);
    state->started = true;
}

void station_stop(void* argument, esp_event_base_t event_base,
                  int32_t event_id, void* event_data) {
    auto* state = static_cast<StationNetifState*>(argument);
    if (!state->started && !esp_netif_is_netif_up(state->netif)) {
        ESP_LOGW(TAG, "ignore duplicate Wi-Fi STA stop event");
        return;
    }
    esp_netif_action_stop(state->netif, event_base, event_id, event_data);
    state->started = false;
}

void station_connected(void* argument, esp_event_base_t event_base,
                       int32_t event_id, void* event_data) {
    auto* state = static_cast<StationNetifState*>(argument);
    auto driver = static_cast<wifi_netif_driver_t>(
        esp_netif_get_io_driver(state->netif));
    if (!esp_wifi_is_if_ready_when_started(driver)) {
        esp_err_t error = esp_wifi_register_if_rxcb(
            driver, esp_netif_receive, state->netif);
        if (error != ESP_OK) {
            ESP_LOGE(TAG, "esp_wifi_register_if_rxcb failed: %s",
                     esp_err_to_name(error));
            return;
        }
    }
    esp_netif_action_connected(state->netif, event_base, event_id, event_data);
}

}  // namespace

esp_netif_t* create_station_netif() {
    esp_netif_config_t config = ESP_NETIF_DEFAULT_WIFI_STA();
    station.netif = esp_netif_new(&config);
    assert(station.netif != nullptr);
    ESP_ERROR_CHECK(esp_netif_attach_wifi_station(station.netif));
    ESP_ERROR_CHECK(esp_event_handler_register(
        WIFI_EVENT, WIFI_EVENT_STA_START, station_start, &station));
    ESP_ERROR_CHECK(esp_event_handler_register(
        WIFI_EVENT, WIFI_EVENT_STA_STOP, station_stop, &station));
    ESP_ERROR_CHECK(esp_event_handler_register(
        WIFI_EVENT, WIFI_EVENT_STA_CONNECTED, station_connected, &station));
    ESP_ERROR_CHECK(esp_event_handler_register(
        WIFI_EVENT, WIFI_EVENT_STA_DISCONNECTED,
        esp_netif_action_disconnected, station.netif));
    ESP_ERROR_CHECK(esp_event_handler_register(
        IP_EVENT, IP_EVENT_STA_GOT_IP, esp_netif_action_got_ip,
        station.netif));
    return station.netif;
}

}  // namespace wifi
