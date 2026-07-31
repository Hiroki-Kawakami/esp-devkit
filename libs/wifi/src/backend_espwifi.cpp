/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <unordered_map>
#include <utility>
#include <vector>

#include "esp_event.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_wifi.h"
#include "wifi_backend.hpp"
#include "wifi_netif.hpp"

namespace wifi {
namespace {

Result reason_to_result(uint8_t reason, bool was_associated) {
    if (was_associated) {
        return Result::IpFailed;
    }
    switch (reason) {
        case WIFI_REASON_NO_AP_FOUND:
            return Result::ApNotFound;
        case WIFI_REASON_AUTH_FAIL:
        case WIFI_REASON_AUTH_EXPIRE:
        case WIFI_REASON_4WAY_HANDSHAKE_TIMEOUT:
        case WIFI_REASON_HANDSHAKE_TIMEOUT:
            return Result::AuthFailed;
        case WIFI_REASON_ASSOC_FAIL:
        case WIFI_REASON_CONNECTION_FAIL:
            return Result::AssocFailed;
        default:
            return Result::Failed;
    }
}

class EspWifiBackend : public Backend {
public:
    explicit EspWifiBackend(BackendHost* host) : host_(host) {}

    void start() override {
        if (started_) {
            return;
        }
        ESP_ERROR_CHECK(esp_netif_init());
        esp_err_t error = esp_event_loop_create_default();
        if (error != ESP_OK && error != ESP_ERR_INVALID_STATE) {
            ESP_ERROR_CHECK(error);
        }
        create_station_netif();

        wifi_init_config_t config = WIFI_INIT_CONFIG_DEFAULT();
        ESP_ERROR_CHECK(esp_wifi_init(&config));
        ESP_ERROR_CHECK(esp_event_handler_instance_register(
            WIFI_EVENT, ESP_EVENT_ANY_ID, &EspWifiBackend::wifi_event,
            this, nullptr));
        ESP_ERROR_CHECK(esp_event_handler_instance_register(
            IP_EVENT, IP_EVENT_STA_GOT_IP, &EspWifiBackend::ip_event,
            this, nullptr));
        ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
        ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
        ESP_ERROR_CHECK(esp_wifi_start());
        started_ = true;
    }

    void radio_on() override {
        if (started_) {
            esp_wifi_start();
        }
    }

    void radio_off() override {
        if (started_) {
            esp_wifi_stop();
        }
    }

    void scan() override {
        wifi_scan_config_t config = {};
        if (esp_wifi_scan_start(&config, false) != ESP_OK) {
            host_->on_scan_result(Result::Failed, {});
        }
    }

    void connect(const std::string& ssid,
                 const std::string& password) override {
        associated_ = false;
        wifi_config_t config = {};
        std::strncpy(reinterpret_cast<char*>(config.sta.ssid), ssid.c_str(),
                     sizeof(config.sta.ssid) - 1);
        std::strncpy(reinterpret_cast<char*>(config.sta.password),
                     password.c_str(), sizeof(config.sta.password) - 1);
        if (esp_wifi_set_config(WIFI_IF_STA, &config) != ESP_OK ||
            esp_wifi_connect() != ESP_OK) {
            host_->on_disconnected(Result::Failed);
        }
    }

    void disconnect() override {
        esp_wifi_disconnect();
    }

    void set_power_save(PowerSave mode) override {
        if (started_) {
            esp_wifi_set_ps(mode == PowerSave::None
                                ? WIFI_PS_NONE
                                : WIFI_PS_MIN_MODEM);
        }
    }

    int8_t rssi() override {
        wifi_ap_record_t info;
        return esp_wifi_sta_get_ap_info(&info) == ESP_OK ? info.rssi : 0;
    }

    std::string mac() override {
        uint8_t address[6];
        if (esp_wifi_get_mac(WIFI_IF_STA, address) != ESP_OK) {
            return {};
        }
        char buffer[18];
        std::snprintf(buffer, sizeof(buffer),
                      "%02x:%02x:%02x:%02x:%02x:%02x", address[0],
                      address[1], address[2], address[3], address[4],
                      address[5]);
        return buffer;
    }

private:
    static void wifi_event(void* argument, esp_event_base_t,
                           int32_t event_id, void* event_data) {
        static_cast<EspWifiBackend*>(argument)->on_wifi_event(
            event_id, event_data);
    }

    static void ip_event(void* argument, esp_event_base_t,
                         int32_t, void* event_data) {
        static_cast<EspWifiBackend*>(argument)->on_ip_event(event_data);
    }

    void on_wifi_event(int32_t event_id, void* event_data) {
        if (event_id == WIFI_EVENT_SCAN_DONE) {
            handle_scan_done();
        } else if (event_id == WIFI_EVENT_STA_CONNECTED) {
            associated_ = true;
            host_->on_associated();
        } else if (event_id == WIFI_EVENT_STA_DISCONNECTED) {
            auto* info = static_cast<wifi_event_sta_disconnected_t*>(event_data);
            bool was_associated = associated_;
            associated_ = false;
            host_->on_disconnected(
                reason_to_result(info->reason, was_associated));
        }
    }

    void handle_scan_done() {
        uint16_t count = 0;
        esp_wifi_scan_get_ap_num(&count);
        std::vector<wifi_ap_record_t> records(count);
        if (count != 0) {
            esp_wifi_scan_get_ap_records(&count, records.data());
        }

        std::unordered_map<std::string, const wifi_ap_record_t*> best;
        for (const auto& record : records) {
            if (record.ssid[0] == '\0') {
                continue;
            }
            std::string ssid(reinterpret_cast<const char*>(record.ssid));
            auto found = best.find(ssid);
            if (found == best.end() || record.rssi > found->second->rssi) {
                best[ssid] = &record;
            }
        }

        std::vector<AP> access_points;
        access_points.reserve(best.size());
        for (const auto& entry : best) {
            access_points.push_back(
                {entry.first, entry.second->rssi,
                 entry.second->authmode != WIFI_AUTH_OPEN});
        }
        std::sort(access_points.begin(), access_points.end(),
                  [](const AP& left, const AP& right) {
                      return left.rssi > right.rssi;
                  });
        host_->on_scan_result(Result::Ok, std::move(access_points));
    }

    void on_ip_event(void* event_data) {
        auto* event = static_cast<ip_event_got_ip_t*>(event_data);
        char address[16];
        std::snprintf(address, sizeof(address), IPSTR,
                      IP2STR(&event->ip_info.ip));
        associated_ = false;
        host_->on_got_ip(address);
    }

    BackendHost* host_;
    bool started_ = false;
    volatile bool associated_ = false;
};

}  // namespace

Backend* make_backend(BackendHost* host) {
    return new EspWifiBackend(host);
}

}  // namespace wifi
