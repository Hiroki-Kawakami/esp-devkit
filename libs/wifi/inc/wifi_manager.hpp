/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <string>
#include <vector>

namespace wifi {

enum class State {
    Off,
    Disconnected,
    Connecting,
    Connected,
};

enum class Result {
    Ok,
    ApNotFound,
    AuthFailed,
    AssocFailed,
    IpFailed,
    Timeout,
    Failed,
};

const char* result_str(Result result);

enum class PowerSave {
    Default,
    None,
};

struct AP {
    std::string ssid;
    int8_t rssi;
    bool secured;
};

struct Status {
    State state = State::Off;
    std::string ssid;
    std::string ip;
    int8_t rssi = 0;
};

class Listener {
public:
    virtual ~Listener() = default;
    virtual void on_wifi_state(const Status& status) = 0;
};

class Manager {
public:
    using ScanCb = std::function<void(Result, std::vector<AP>)>;
    using ConnectCb = std::function<void(Result)>;

    void start(std::weak_ptr<Listener> listener);
    void set_listener(std::weak_ptr<Listener> listener);
    void autoconnect_saved(int timeout_ms = 15000);

    bool configured() const;
    std::string saved_ssid() const;
    Status status() const;
    std::string mac_address() const;

    void scan(ScanCb callback);
    void connect(const std::string& ssid, const std::string& password,
                 ConnectCb callback, int timeout_ms = 15000);
    void connect_saved(ConnectCb callback, int timeout_ms = 15000);

    void set_enabled(bool enabled, std::function<void()> done = {});
    bool enabled() const;
    void set_power_save(PowerSave mode);
    void disconnect();
    void forget();

    Manager(const Manager&) = delete;
    Manager& operator=(const Manager&) = delete;

private:
    friend Manager& manager();
    Manager();
    void bring_up();
    void apply_enabled(bool enabled);
    void do_autoconnect(int timeout_ms);
    void worker_loop();
    static void worker_trampoline(void* argument);

    struct Impl;
    std::unique_ptr<Impl> p_;
};

Manager& manager();

}  // namespace wifi
