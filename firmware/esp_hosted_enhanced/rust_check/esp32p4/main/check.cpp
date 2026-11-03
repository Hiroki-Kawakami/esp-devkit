/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "check.h"

#include <cstring>
#include <memory>

#include "ble_bench.hpp"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "harness.h"
#include "netbench.hpp"
#include "wifi_manager.hpp"

namespace {

const char* TAG = "rust_check";

constexpr int kJoinTimeoutMs = 20000;

const char* state_str(wifi::State state) {
    switch (state) {
    case wifi::State::Off: return "off";
    case wifi::State::Disconnected: return "disconnected";
    case wifi::State::Connecting: return "connecting";
    case wifi::State::Connected: return "connected";
    }
    return "?";
}

class Listener : public wifi::Listener {
public:
    void on_wifi_state(const wifi::Status& status) override {
        ESP_LOGI(TAG, "wifi %s %s", state_str(status.state), status.ip.c_str());
        if (status.state == wifi::State::Connected) netbench_start();
    }
};

bool join(const char* ssid, const char* password) {
    static SemaphoreHandle_t done = xSemaphoreCreateBinary();
    static wifi::Result result;
    xSemaphoreTake(done, 0);
    wifi::manager().connect(ssid, password, [](wifi::Result r) {
        result = r;
        xSemaphoreGive(done);
    }, kJoinTimeoutMs);
    if (xSemaphoreTake(done, pdMS_TO_TICKS(kJoinTimeoutMs + 5000)) != pdTRUE) {
        harness_reply("ERR wifi join timed out");
        return true;
    }
    harness_reply("OK wifi %d", static_cast<int>(result));
    return true;
}

bool cmd_wifi(int argc, const char* const* argv, void*) {
    if (argc == 3 && std::strcmp(argv[1], "ps") == 0) {
        if (std::strcmp(argv[2], "none") == 0) {
            wifi::manager().set_power_save(wifi::PowerSave::None);
        } else if (std::strcmp(argv[2], "default") == 0) {
            wifi::manager().set_power_save(wifi::PowerSave::Default);
        } else {
            return false;
        }
        return true;
    }
    if (argc == 4 && std::strcmp(argv[1], "join") == 0) return join(argv[2], argv[3]);
    if (argc != 1) return false;
    wifi::Status status = wifi::manager().status();
    harness_reply("OK wifi %s %s %d", state_str(status.state),
                  status.ip.empty() ? "-" : status.ip.c_str(), status.rssi);
    return true;
}

}  // namespace

void check_start() {
    harness_register("wifi", cmd_wifi, nullptr);
    ble_bench_start();
}

void check_wifi_autoconnect() {
    static auto listener = std::make_shared<Listener>();
    wifi::manager().set_listener(listener);
    wifi::manager().autoconnect_saved();
}
