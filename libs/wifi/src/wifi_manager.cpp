/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi_manager.hpp"

#include <mutex>
#include <utility>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "freertos/timers.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "wifi_backend.hpp"

namespace wifi {

static const char* TAG = "wifi";
static constexpr char kNvsNamespace[] = "wifi";

namespace {

enum class Op {
    Enable,
    Disable,
    Autoconnect,
};

struct WorkItem {
    Op op;
    int timeout_ms;
    std::function<void()> done;
};

void enqueue(QueueHandle_t queue, WorkItem* item) {
    if (queue == nullptr || xQueueSend(queue, &item, 0) != pdTRUE) {
        if (item->done) {
            item->done();
        }
        delete item;
    }
}

}  // namespace

const char* result_str(Result result) {
    switch (result) {
        case Result::Ok:
            return "Ok";
        case Result::ApNotFound:
            return "ApNotFound";
        case Result::AuthFailed:
            return "AuthFailed";
        case Result::AssocFailed:
            return "AssocFailed";
        case Result::IpFailed:
            return "IpFailed";
        case Result::Timeout:
            return "Timeout";
        case Result::Failed:
            return "Failed";
    }
    return "?";
}

struct Manager::Impl : BackendHost {
    mutable std::mutex mutex;
    Backend* backend = nullptr;
    bool started = false;
    QueueHandle_t command_queue = nullptr;
    TaskHandle_t worker = nullptr;
    std::weak_ptr<Listener> listener;
    State state = State::Off;
    std::string ssid;
    std::string ip;
    bool connecting = false;
    ConnectCb connect_callback;
    ScanCb scan_callback;
    TimerHandle_t timeout_timer = nullptr;

    Status snapshot_locked() const {
        Status result;
        result.state = state;
        result.ssid = ssid;
        result.ip = state == State::Connected ? ip : std::string();
        result.rssi = state == State::Connected && backend != nullptr
                          ? backend->rssi()
                          : 0;
        return result;
    }

    void notify(const Status& status) {
        std::shared_ptr<Listener> target;
        {
            std::lock_guard<std::mutex> lock(mutex);
            target = listener.lock();
        }
        if (target) {
            target->on_wifi_state(status);
        }
    }

    void arm_timeout(int timeout_ms) {
        if (timeout_timer == nullptr) {
            return;
        }
        xTimerChangePeriod(timeout_timer, pdMS_TO_TICKS(timeout_ms), 0);
        xTimerStart(timeout_timer, 0);
    }

    void cancel_timeout() {
        if (timeout_timer != nullptr) {
            xTimerStop(timeout_timer, 0);
        }
    }

    void on_scan_result(Result result, std::vector<AP> access_points) override {
        ScanCb callback;
        {
            std::lock_guard<std::mutex> lock(mutex);
            callback = std::move(scan_callback);
            scan_callback = nullptr;
        }
        if (callback) {
            callback(result, std::move(access_points));
        }
    }

    void on_associated() override {}

    void on_got_ip(const std::string& got_ip) override {
        ConnectCb callback;
        Status status;
        {
            std::lock_guard<std::mutex> lock(mutex);
            ip = got_ip;
            state = State::Connected;
            cancel_timeout();
            if (connecting) {
                connecting = false;
                callback = std::move(connect_callback);
                connect_callback = nullptr;
            }
            status = snapshot_locked();
        }
        ESP_LOGI(TAG, "connected: %s", status.ssid.c_str());
        if (callback) {
            callback(Result::Ok);
        }
        notify(status);
    }

    void on_disconnected(Result reason) override {
        ConnectCb callback;
        Status status;
        bool was_connecting;
        {
            std::lock_guard<std::mutex> lock(mutex);
            was_connecting = connecting;
            ip.clear();
            if (state != State::Off) {
                state = State::Disconnected;
            }
            cancel_timeout();
            if (connecting) {
                connecting = false;
                callback = std::move(connect_callback);
                connect_callback = nullptr;
            }
            status = snapshot_locked();
        }
        if (was_connecting) {
            ESP_LOGW(TAG, "connect failed: %s", result_str(reason));
        } else {
            ESP_LOGW(TAG, "link dropped");
        }
        if (callback) {
            callback(reason);
        }
        notify(status);
    }

    static void timeout_callback(TimerHandle_t timer) {
        auto* self = static_cast<Impl*>(pvTimerGetTimerID(timer));
        bool should_disconnect;
        {
            std::lock_guard<std::mutex> lock(self->mutex);
            should_disconnect = self->connecting;
        }
        if (should_disconnect && self->backend != nullptr) {
            self->backend->disconnect();
        }

        ConnectCb callback;
        Status status;
        {
            std::lock_guard<std::mutex> lock(self->mutex);
            if (!self->connecting) {
                return;
            }
            self->connecting = false;
            self->state = State::Disconnected;
            callback = std::move(self->connect_callback);
            self->connect_callback = nullptr;
            status = self->snapshot_locked();
        }
        ESP_LOGW(TAG, "connect timed out");
        if (callback) {
            callback(Result::Timeout);
        }
        self->notify(status);
    }

    bool load_credentials(std::string& loaded_ssid,
                          std::string& loaded_password) const {
        nvs_handle_t handle;
        if (nvs_open(kNvsNamespace, NVS_READONLY, &handle) != ESP_OK) {
            return false;
        }

        char buffer[128];
        size_t length = sizeof(buffer);
        bool valid = nvs_get_str(handle, "ssid", buffer, &length) == ESP_OK &&
                     buffer[0] != '\0';
        if (valid) {
            loaded_ssid.assign(buffer);
            length = sizeof(buffer);
            loaded_password = nvs_get_str(handle, "pass", buffer, &length) == ESP_OK
                                  ? buffer
                                  : "";
        }
        nvs_close(handle);
        return valid;
    }

    void save_credentials(const std::string& saved_ssid,
                          const std::string& saved_password) {
        nvs_handle_t handle;
        if (nvs_open(kNvsNamespace, NVS_READWRITE, &handle) != ESP_OK) {
            return;
        }
        nvs_set_str(handle, "ssid", saved_ssid.c_str());
        nvs_set_str(handle, "pass", saved_password.c_str());
        nvs_commit(handle);
        nvs_close(handle);
    }

    void erase_credentials() {
        nvs_handle_t handle;
        if (nvs_open(kNvsNamespace, NVS_READWRITE, &handle) != ESP_OK) {
            return;
        }
        nvs_erase_all(handle);
        nvs_commit(handle);
        nvs_close(handle);
    }
};

Manager::Manager() : p_(new Impl) {
    p_->command_queue = xQueueCreate(8, sizeof(WorkItem*));
    xTaskCreate(&Manager::worker_trampoline, "wifi_mgr", 8192, this, 4,
                &p_->worker);
}

void Manager::worker_trampoline(void* argument) {
    static_cast<Manager*>(argument)->worker_loop();
}

void Manager::worker_loop() {
    for (;;) {
        WorkItem* item = nullptr;
        if (xQueueReceive(p_->command_queue, &item, portMAX_DELAY) != pdTRUE ||
            item == nullptr) {
            continue;
        }
        switch (item->op) {
            case Op::Enable:
                bring_up();
                apply_enabled(true);
                do_autoconnect(item->timeout_ms);
                break;
            case Op::Disable:
                apply_enabled(false);
                break;
            case Op::Autoconnect:
                bring_up();
                do_autoconnect(item->timeout_ms);
                break;
        }
        if (item->done) {
            item->done();
        }
        delete item;
    }
}

void Manager::set_listener(std::weak_ptr<Listener> listener) {
    std::lock_guard<std::mutex> lock(p_->mutex);
    p_->listener = std::move(listener);
}

void Manager::start(std::weak_ptr<Listener> listener) {
    set_listener(std::move(listener));
    enqueue(p_->command_queue, new WorkItem{Op::Enable, 0, {}});
}

void Manager::bring_up() {
    bool first;
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        first = !p_->started;
        p_->started = true;
    }
    if (!first) {
        return;
    }

    esp_err_t error = nvs_flash_init();
    if (error == ESP_ERR_NVS_NO_FREE_PAGES ||
        error == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        nvs_flash_erase();
        nvs_flash_init();
    }

    p_->timeout_timer = xTimerCreate(
        "wifi_to", pdMS_TO_TICKS(15000), pdFALSE, p_.get(),
        &Impl::timeout_callback);
    p_->backend = make_backend(p_.get());
    p_->backend->start();
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        if (p_->state == State::Off) {
            p_->state = State::Disconnected;
        }
    }
}

void Manager::autoconnect_saved(int timeout_ms) {
    enqueue(p_->command_queue,
            new WorkItem{Op::Autoconnect, timeout_ms, {}});
}

void Manager::do_autoconnect(int timeout_ms) {
    std::string ssid;
    std::string password;
    if (!p_->load_credentials(ssid, password)) {
        ESP_LOGI(TAG, "autoconnect: no saved network");
        return;
    }
    connect(ssid, password, [](Result) {}, timeout_ms);
}

bool Manager::configured() const {
    std::string ssid;
    std::string password;
    return p_->load_credentials(ssid, password);
}

std::string Manager::saved_ssid() const {
    std::string ssid;
    std::string password;
    return p_->load_credentials(ssid, password) ? ssid : std::string();
}

Status Manager::status() const {
    std::lock_guard<std::mutex> lock(p_->mutex);
    return p_->snapshot_locked();
}

std::string Manager::mac_address() const {
    std::lock_guard<std::mutex> lock(p_->mutex);
    return p_->backend != nullptr ? p_->backend->mac() : std::string();
}

void Manager::scan(ScanCb callback) {
    ScanCb superseded;
    Backend* backend;
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        superseded = std::move(p_->scan_callback);
        p_->scan_callback = std::move(callback);
        backend = p_->backend;
    }
    if (superseded) {
        superseded(Result::Failed, {});
    }
    if (backend != nullptr) {
        backend->scan();
    } else {
        p_->on_scan_result(Result::Failed, {});
    }
}

void Manager::connect(const std::string& ssid, const std::string& password,
                      ConnectCb callback, int timeout_ms) {
    ConnectCb superseded;
    Backend* backend;
    Status current;
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        superseded = std::move(p_->connect_callback);
        p_->connect_callback = std::move(callback);
        p_->connecting = true;
        p_->ssid = ssid;
        p_->ip.clear();
        p_->state = State::Connecting;
        backend = p_->backend;
        current = p_->snapshot_locked();
    }
    if (superseded) {
        superseded(Result::Failed);
    }
    p_->notify(current);

    if (backend == nullptr) {
        p_->on_disconnected(Result::Failed);
        return;
    }
    p_->save_credentials(ssid, password);
    p_->arm_timeout(timeout_ms);
    backend->connect(ssid, password);
}

void Manager::connect_saved(ConnectCb callback, int timeout_ms) {
    std::string ssid;
    std::string password;
    if (!p_->load_credentials(ssid, password)) {
        callback(Result::Failed);
        return;
    }
    connect(ssid, password, std::move(callback), timeout_ms);
}

void Manager::set_enabled(bool enabled, std::function<void()> done) {
    enqueue(p_->command_queue,
            new WorkItem{enabled ? Op::Enable : Op::Disable, 15000,
                         std::move(done)});
}

void Manager::apply_enabled(bool enabled) {
    ConnectCb pending;
    Backend* backend;
    Status current;
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        backend = p_->backend;
        if (backend == nullptr) {
            return;
        }
        bool is_off = p_->state == State::Off;
        if (enabled == !is_off) {
            return;
        }
        if (enabled) {
            p_->state = State::Disconnected;
        } else {
            p_->cancel_timeout();
            if (p_->connecting) {
                p_->connecting = false;
                pending = std::move(p_->connect_callback);
                p_->connect_callback = nullptr;
            }
            p_->ip.clear();
            p_->ssid.clear();
            p_->state = State::Off;
        }
        current = p_->snapshot_locked();
    }

    if (enabled) {
        backend->radio_on();
    } else {
        backend->radio_off();
    }
    if (pending) {
        pending(Result::Failed);
    }
    p_->notify(current);
}

bool Manager::enabled() const {
    std::lock_guard<std::mutex> lock(p_->mutex);
    return p_->state != State::Off;
}

void Manager::set_power_save(PowerSave mode) {
    Backend* backend;
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        backend = p_->backend;
    }
    if (backend != nullptr) {
        backend->set_power_save(mode);
    }
}

void Manager::disconnect() {
    Backend* backend;
    Status current;
    {
        std::lock_guard<std::mutex> lock(p_->mutex);
        p_->cancel_timeout();
        p_->connecting = false;
        p_->connect_callback = nullptr;
        p_->ip.clear();
        p_->state = State::Disconnected;
        backend = p_->backend;
        current = p_->snapshot_locked();
    }
    if (backend != nullptr) {
        backend->disconnect();
    }
    p_->notify(current);
}

void Manager::forget() {
    p_->erase_credentials();
}

Manager& manager() {
    static Manager* instance = new Manager();
    return *instance;
}

}  // namespace wifi
