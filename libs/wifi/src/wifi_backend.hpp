/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "wifi_manager.hpp"

namespace wifi {

class BackendHost {
public:
    virtual ~BackendHost() = default;
    virtual void on_scan_result(Result result, std::vector<AP> access_points) = 0;
    virtual void on_associated() = 0;
    virtual void on_got_ip(const std::string& ip) = 0;
    virtual void on_disconnected(Result reason) = 0;
};

class Backend {
public:
    virtual ~Backend() = default;
    virtual void start() = 0;
    virtual void radio_on() = 0;
    virtual void radio_off() = 0;
    virtual void scan() = 0;
    virtual void connect(const std::string& ssid,
                         const std::string& password) = 0;
    virtual void disconnect() = 0;
    virtual void set_power_save(PowerSave mode) = 0;
    virtual int8_t rssi() = 0;
    virtual std::string mac() = 0;
};

Backend* make_backend(BackendHost* host);

}  // namespace wifi
