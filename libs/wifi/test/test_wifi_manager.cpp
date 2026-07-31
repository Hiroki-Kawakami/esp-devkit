/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <cassert>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include "nvs_flash.h"
#include "wifi_manager.hpp"
#include "wifi_sim.hpp"

namespace {

class Listener : public wifi::Listener {
public:
    void on_wifi_state(const wifi::Status& status) override {
        std::lock_guard<std::mutex> lock(mutex_);
        latest_ = status;
        changed_.notify_all();
    }

    bool wait_for(wifi::State state) {
        std::unique_lock<std::mutex> lock(mutex_);
        return changed_.wait_for(lock, std::chrono::seconds(2), [&] {
            return latest_.state == state;
        });
    }

private:
    std::mutex mutex_;
    std::condition_variable changed_;
    wifi::Status latest_;
};

}  // namespace

bool wait_for_status(wifi::Manager& manager, wifi::State state) {
    for (int attempt = 0; attempt < 200; ++attempt) {
        if (manager.status().state == state) {
            return true;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    return false;
}

int main(int argc, char** argv) {
    assert(argc == 2);
    nvs_flash_sim_set_path(argv[1]);

    auto listener = std::make_shared<Listener>();
    auto& manager = wifi::manager();
    wifi::sim::set_event_delay_ms(0);
    manager.start(listener);
    assert(wait_for_status(manager, wifi::State::Disconnected));

    std::mutex mutex;
    std::condition_variable changed;
    bool scan_done = false;
    std::vector<wifi::AP> scanned;
    wifi::sim::set_aps({
        {"YOUR_SSID", -30, true},
        {"YOUR_SECOND_SSID", -60, false},
    });
    manager.scan([&](wifi::Result result, std::vector<wifi::AP> aps) {
        std::lock_guard<std::mutex> lock(mutex);
        assert(result == wifi::Result::Ok);
        scanned = std::move(aps);
        scan_done = true;
        changed.notify_all();
    });
    {
        std::unique_lock<std::mutex> lock(mutex);
        assert(changed.wait_for(lock, std::chrono::seconds(2),
                                [&] { return scan_done; }));
    }
    assert(scanned.size() == 2);
    assert(scanned.front().ssid == "YOUR_SSID");

    bool connect_done = false;
    wifi::Result connect_result = wifi::Result::Failed;
    manager.connect("YOUR_SSID", "YOUR_PASSWORD", [&](wifi::Result result) {
        std::lock_guard<std::mutex> lock(mutex);
        connect_result = result;
        connect_done = true;
        changed.notify_all();
    });
    {
        std::unique_lock<std::mutex> lock(mutex);
        assert(changed.wait_for(lock, std::chrono::seconds(2),
                                [&] { return connect_done; }));
    }
    assert(connect_result == wifi::Result::Ok);
    assert(listener->wait_for(wifi::State::Connected));
    assert(manager.configured());
    assert(manager.saved_ssid() == "YOUR_SSID");

    manager.set_enabled(false);
    assert(listener->wait_for(wifi::State::Off));
    manager.forget();
    assert(!manager.configured());

    std::puts("wifi manager simulator tests passed");
    return 0;
}
