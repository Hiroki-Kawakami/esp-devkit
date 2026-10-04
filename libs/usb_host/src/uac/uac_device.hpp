/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <vector>

#include "transfer.hpp"
#include "uac_descriptors.hpp"
#include "uac_stream.hpp"
#include "usb_host_uac.hpp"

namespace usb_host::detail {

class UacOutputDevice final : public UacDevice {
public:
    static esp_err_t open(usb_host_client_handle_t client, uint8_t address,
                          std::shared_ptr<UacOutputDevice>* out);
    ~UacOutputDevice() override;

    bool connected() const override { return !gone_; }
    const std::vector<UacFormat>& formats() const override { return formats_; }
    esp_err_t open(size_t format, uint32_t rate) override;
    void close() override;
    esp_err_t write(const void* data, size_t len) override;
    void drain() override;

    bool has_volume() const override { return volume_channels_ != 0; }
    bool has_mute() const override { return mute_channels_ != 0; }
    float volume_min_db() const override { return volume_min_ / 256.0f; }
    float volume_max_db() const override { return volume_max_ / 256.0f; }
    void set_volume_db(float db) override;
    void set_mute(bool mute) override;

    usb_device_handle_t usb_device() const { return xfer_.device(); }
    void mark_gone();

private:
    UacOutputDevice() = default;
    esp_err_t setup(usb_host_client_handle_t client, uint8_t address);
    void probe_volume();
    esp_err_t control_out(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                          const void* data, uint16_t length);
    esp_err_t control_in(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                         void* data, uint16_t length);
    void close_locked();

    static void feature_done(usb_transfer_t* transfer);
    void submit_feature_locked();
    uint16_t feature_index() const;

    TransferContext xfer_;
    usb_transfer_t* ctrl_ = nullptr;
    UacTopology topology_;
    std::vector<UacFormat> formats_;
    IsocOutStream stream_;
    std::mutex lock_;
    std::atomic<bool> gone_{false};
    usb_speed_t speed_ = USB_SPEED_FULL;
    int active_ = -1;

    usb_transfer_t* feature_ = nullptr;
    std::mutex feature_lock_;
    bool feature_busy_ = false;
    bool closing_ = false;
    uint8_t mute_channels_ = 0;
    uint8_t volume_channels_ = 0;
    uint8_t mute_pending_ = 0;
    uint8_t volume_pending_ = 0;
    bool mute_known_ = false;
    bool volume_known_ = false;
    bool mute_ = false;
    int16_t volume_ = 0;
    int16_t volume_min_ = 0;
    int16_t volume_max_ = 0;
    int16_t volume_res_ = 1;
};

}  // namespace usb_host::detail
