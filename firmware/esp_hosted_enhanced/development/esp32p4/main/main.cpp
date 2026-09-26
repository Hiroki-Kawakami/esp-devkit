/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp_hosted_enhanced development firmware: joins the saved network and
 * serves netbench.py. Harness commands:
 *   wifi                 -> #OK wifi <state> <ip|-> <rssi>
 *   wifi ps none|default -> #OK wifi
 *   heap                 -> #OK heap <int_free> <int_min> <int_largest> <dma_free> <psram_free> <int_free_at_main>
 */

#include <cstring>
#include <memory>

#include "bsp.h"
#include "c6_flash.hpp"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "harness.h"
#include "netbench.hpp"
#include "sdio_bench.hpp"
#include "wifi_manager.hpp"

namespace {

const char* TAG = "hosted_dev";

size_t s_int_free_at_main;

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
    wifi::Status status = wifi::manager().status();
    harness_reply("OK wifi %s %s %d", state_str(status.state),
                  status.ip.empty() ? "-" : status.ip.c_str(), status.rssi);
    return true;
}

bool cmd_heap(int, const char* const*, void*) {
    harness_reply("OK heap %u %u %u %u %u %u",
                  (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
                  (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL),
                  (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL),
                  (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_DMA),
                  (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM),
                  (unsigned)s_int_free_at_main);
    return true;
}

}  // namespace

extern "C" void app_main() {
    ESP_ERROR_CHECK(bsp_init(nullptr));
    harness_register("wifi", cmd_wifi, nullptr);
    harness_register("heap", cmd_heap, nullptr);
    sdio_bench_start();
    ESP_ERROR_CHECK(harness_start());

    s_int_free_at_main = heap_caps_get_free_size(MALLOC_CAP_INTERNAL);
    if (c6_flash_start()) return;
    static auto listener = std::make_shared<Listener>();
    wifi::manager().set_listener(listener);
    wifi::manager().autoconnect_saved();
}
