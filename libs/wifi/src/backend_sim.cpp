/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "esp_log.h"
#include "sim_harness.h"
#include "wifi_backend.hpp"
#include "wifi_sim.hpp"

namespace wifi {
namespace {

static const char* TAG = "wifi.sim";

struct Fake {
    std::mutex mutex;
    std::vector<AP> access_points = {
        {"YOUR_SSID", -45, true},
        {"YOUR_GUEST_SSID", -60, true},
        {"YOUR_OPEN_SSID", -72, false},
    };
    bool has_forced_result = false;
    Result forced_result = Result::Ok;
    int delay_ms = 300;
    std::string ip = "192.168.x.x";
    int8_t current_rssi = 0;
    BackendHost* host = nullptr;
};

Fake& fake() {
    static Fake* instance = new Fake();
    return *instance;
}

template <typename Function>
void async_after(int delay_ms, Function function) {
    std::thread([delay_ms, function = std::move(function)]() mutable {
        std::this_thread::sleep_for(std::chrono::milliseconds(delay_ms));
        function();
    }).detach();
}

Result heuristic_result(const std::string& ssid) {
    if (ssid.find("fail-auth") != std::string::npos) {
        return Result::AuthFailed;
    }
    if (ssid.find("fail-notfound") != std::string::npos) {
        return Result::ApNotFound;
    }
    if (ssid.find("fail-assoc") != std::string::npos) {
        return Result::AssocFailed;
    }
    return Result::Ok;
}

bool parse_result(const char* value, Result& result) {
    struct Entry {
        const char* name;
        Result result;
    };
    const Entry entries[] = {
        {"Ok", Result::Ok},
        {"ApNotFound", Result::ApNotFound},
        {"AuthFailed", Result::AuthFailed},
        {"AssocFailed", Result::AssocFailed},
        {"IpFailed", Result::IpFailed},
        {"Timeout", Result::Timeout},
        {"Failed", Result::Failed},
    };
    for (const auto& entry : entries) {
        if (std::strcmp(value, entry.name) == 0) {
            result = entry.result;
            return true;
        }
    }
    return false;
}

std::vector<AP> parse_access_points(const char* value) {
    std::vector<AP> result;
    std::string specification(value);
    size_t begin = 0;
    while (begin < specification.size()) {
        size_t comma = specification.find(',', begin);
        std::string entry = specification.substr(
            begin, comma == std::string::npos
                       ? std::string::npos
                       : comma - begin);
        size_t first = entry.find(':');
        size_t last = entry.rfind(':');
        if (first != std::string::npos && last != first) {
            AP access_point;
            access_point.ssid = entry.substr(0, first);
            access_point.rssi = static_cast<int8_t>(std::atoi(
                entry.substr(first + 1, last - first - 1).c_str()));
            access_point.secured =
                std::atoi(entry.substr(last + 1).c_str()) != 0;
            result.push_back(std::move(access_point));
        }
        if (comma == std::string::npos) {
            break;
        }
        begin = comma + 1;
    }
    return result;
}

bool harness_command(int argc, const char* const* argv, void*) {
    if (argc < 1) {
        return true;
    }
    if (std::strcmp(argv[0], "wifi-aps") == 0 && argc >= 2) {
        sim::set_aps(parse_access_points(argv[1]));
    } else if (std::strcmp(argv[0], "wifi-connect-result") == 0 &&
               argc >= 2) {
        Result result;
        if (parse_result(argv[1], result)) {
            sim::set_next_connect_result(result);
        }
    } else if (std::strcmp(argv[0], "wifi-delay") == 0 && argc >= 2) {
        sim::set_event_delay_ms(std::atoi(argv[1]));
    } else if (std::strcmp(argv[0], "wifi-drop") == 0) {
        sim::drop_link();
    }
    return true;
}

class SimBackend : public Backend {
public:
    explicit SimBackend(BackendHost* host) {
        std::lock_guard<std::mutex> lock(fake().mutex);
        fake().host = host;
    }

    void start() override {
        if (const char* value = std::getenv("SIMULATOR_WIFI_CONNECT")) {
            Result result;
            if (parse_result(value, result)) {
                sim::set_next_connect_result(result);
            }
        }
        ESP_LOGI(TAG, "fake Wi-Fi backend up");
    }

    void radio_on() override {}

    void radio_off() override {
        std::lock_guard<std::mutex> lock(fake().mutex);
        fake().current_rssi = 0;
    }

    void scan() override {
        std::vector<AP> access_points;
        int delay_ms;
        {
            std::lock_guard<std::mutex> lock(fake().mutex);
            access_points = fake().access_points;
            delay_ms = fake().delay_ms;
        }
        async_after(delay_ms,
                    [access_points = std::move(access_points)]() mutable {
                        fake().host->on_scan_result(
                            Result::Ok, std::move(access_points));
                    });
    }

    void connect(const std::string& ssid, const std::string&) override {
        Result result;
        int delay_ms;
        int8_t rssi = -50;
        std::string ip;
        {
            std::lock_guard<std::mutex> lock(fake().mutex);
            if (fake().has_forced_result) {
                result = fake().forced_result;
                fake().has_forced_result = false;
            } else {
                result = heuristic_result(ssid);
            }
            if (result == Result::Timeout ||
                ssid.find("timeout") != std::string::npos) {
                return;
            }
            delay_ms = fake().delay_ms;
            ip = fake().ip;
            for (const auto& access_point : fake().access_points) {
                if (access_point.ssid == ssid) {
                    rssi = access_point.rssi;
                }
            }
            fake().current_rssi = result == Result::Ok ? rssi : 0;
        }
        async_after(delay_ms, [result, ip = std::move(ip)]() {
            BackendHost* host;
            {
                std::lock_guard<std::mutex> lock(fake().mutex);
                host = fake().host;
            }
            if (result == Result::Ok) {
                host->on_associated();
                host->on_got_ip(ip);
            } else {
                host->on_disconnected(result);
            }
        });
    }

    void disconnect() override {
        std::lock_guard<std::mutex> lock(fake().mutex);
        fake().current_rssi = 0;
    }

    void set_power_save(PowerSave) override {}

    int8_t rssi() override {
        std::lock_guard<std::mutex> lock(fake().mutex);
        return fake().current_rssi;
    }

    std::string mac() override {
        return "XX:XX:XX:XX:XX:XX";
    }
};

}  // namespace

namespace sim {

void set_aps(std::vector<AP> access_points) {
    std::sort(access_points.begin(), access_points.end(),
              [](const AP& left, const AP& right) {
                  return left.rssi > right.rssi;
              });
    std::lock_guard<std::mutex> lock(fake().mutex);
    fake().access_points = std::move(access_points);
}

void set_next_connect_result(Result result) {
    std::lock_guard<std::mutex> lock(fake().mutex);
    fake().has_forced_result = true;
    fake().forced_result = result;
}

void set_event_delay_ms(int delay_ms) {
    std::lock_guard<std::mutex> lock(fake().mutex);
    fake().delay_ms = delay_ms;
}

void set_ip(std::string ip) {
    std::lock_guard<std::mutex> lock(fake().mutex);
    fake().ip = std::move(ip);
}

void drop_link() {
    BackendHost* host;
    {
        std::lock_guard<std::mutex> lock(fake().mutex);
        host = fake().host;
        fake().current_rssi = 0;
    }
    if (host != nullptr) {
        host->on_disconnected(Result::Failed);
    }
}

void register_harness_commands() {
    const char* names[] = {
        "wifi-aps",
        "wifi-connect-result",
        "wifi-delay",
        "wifi-drop",
    };
    for (const char* name : names) {
        sim_harness_register(name, harness_command, nullptr);
    }
}

}  // namespace sim

Backend* make_backend(BackendHost* host) {
    return new SimBackend(host);
}

}  // namespace wifi
