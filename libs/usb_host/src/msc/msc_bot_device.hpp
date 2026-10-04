/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <mutex>

#include "esp_blockdev.h"
#include "transfer.hpp"
#include "usb_host_msc.hpp"

namespace usb_host::detail {

class MscBotDevice final : public MscDevice {
public:
    static esp_err_t open(usb_host_client_handle_t client, uint8_t address,
                          std::shared_ptr<MscBotDevice>* out);
    ~MscBotDevice() override;

    bool connected() const override { return !gone_; }
    uint32_t block_size() const override { return block_size_; }
    uint32_t block_count() const override { return block_count_; }
    esp_err_t read(void* dst, uint32_t block, uint32_t count) override;
    esp_err_t write(const void* src, uint32_t block, uint32_t count) override;

    usb_device_handle_t usb_device() const { return xfer_.device(); }
    void mark_gone() { gone_ = true; }
    esp_blockdev_handle_t blockdev() { return &blockdev_; }

private:
    MscBotDevice() = default;
    esp_err_t setup(usb_host_client_handle_t client, uint8_t address);

    esp_err_t control_transfer(uint8_t request_type, uint8_t request, uint16_t value,
                               uint16_t index, uint16_t length);
    esp_err_t clear_halt(uint8_t endpoint);
    esp_err_t bulk_transfer(usb_transfer_t* transfer, uint8_t endpoint, size_t bytes);
    esp_err_t mass_storage_reset();
    esp_err_t data_stage(void* data, size_t bytes, bool in);
    esp_err_t run_command(const uint8_t* cb, uint8_t cb_length, void* data, size_t bytes,
                          bool in);
    esp_err_t scsi_request_sense(uint8_t* sense_key);
    esp_err_t scsi_test_unit_ready();
    esp_err_t scsi_inquiry();
    esp_err_t scsi_read_capacity();
    esp_err_t wait_until_ready();
    esp_err_t transfer_blocks(void* buffer, uint32_t block, uint32_t count, bool read);
    esp_err_t transfer_chunked(void* buffer, uint32_t block, uint32_t count, bool read);
    esp_err_t find_endpoints(const usb_config_desc_t* config, const usb_intf_desc_t* interface,
                             int offset);
    void init_blockdev();

    TransferContext xfer_;
    std::mutex lock_;
    std::atomic<bool> gone_{false};
    usb_transfer_t* cmd_ = nullptr;
    usb_transfer_t* data_ = nullptr;
    uint8_t* bounce_ = nullptr;
    size_t bounce_bytes_ = 0;
    uint32_t tag_ = 0;
    uint32_t block_size_ = 0;
    uint32_t block_count_ = 0;
    uint8_t interface_ = 0;
    uint8_t bulk_in_ = 0;
    uint8_t bulk_out_ = 0;
    uint16_t bulk_in_mps_ = 0;
    uint16_t bulk_out_mps_ = 0;
    bool claimed_ = false;
    esp_blockdev_t blockdev_ = {};
};

}  // namespace usb_host::detail
