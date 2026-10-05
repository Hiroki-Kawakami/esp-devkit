/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "msc_bot_device.hpp"

#include <cstring>
#include <new>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "host_internal.hpp"
#include "usb/usb_helpers.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_msc";

constexpr uint8_t kSubclassScsi = 0x06;
constexpr uint8_t kProtocolBot = 0x50;
constexpr uint8_t kRequestReset = 0xFF;

constexpr uint32_t kCbwSignature = 0x43425355;
constexpr uint32_t kCswSignature = 0x53425355;
constexpr uint8_t kCbwFlagDirIn = 0x80;
constexpr size_t kCbwBytes = 31;
constexpr size_t kCswBytes = 13;

constexpr uint8_t kScsiTestUnitReady = 0x00;
constexpr uint8_t kScsiRequestSense = 0x03;
constexpr uint8_t kScsiInquiry = 0x12;
constexpr uint8_t kScsiReadCapacity10 = 0x25;
constexpr uint8_t kScsiRead10 = 0x28;
constexpr uint8_t kScsiWrite10 = 0x2A;

constexpr uint8_t kSenseNoSense = 0x00;
constexpr uint8_t kSenseNotReady = 0x02;
constexpr uint8_t kSenseUnitAttention = 0x06;

constexpr uint32_t kTransferTimeoutMs = 5000;
constexpr uint32_t kReadyTimeoutMs = 3000;
constexpr size_t kCommandBytes = 512;
constexpr size_t kBounceBytes = 4096;
constexpr uint32_t kMaxBlocksPerCommand = 65535;

struct __attribute__((packed)) Cbw {
    uint32_t signature;
    uint32_t tag;
    uint32_t data_length;
    uint8_t flags;
    uint8_t lun;
    uint8_t cb_length;
    uint8_t cb[16];
};

struct __attribute__((packed)) Csw {
    uint32_t signature;
    uint32_t tag;
    uint32_t residue;
    uint8_t status;
};

uint32_t be32(const uint8_t* bytes) {
    return (static_cast<uint32_t>(bytes[0]) << 24) | (static_cast<uint32_t>(bytes[1]) << 16) |
           (static_cast<uint32_t>(bytes[2]) << 8) | bytes[3];
}

const usb_intf_desc_t* find_interface(const usb_config_desc_t* config, int* offset) {
    auto* desc = reinterpret_cast<const usb_standard_desc_t*>(config);
    while ((desc = usb_parse_next_descriptor_of_type(desc, config->wTotalLength,
                                                     USB_W_VALUE_DT_INTERFACE, offset))) {
        auto* interface = reinterpret_cast<const usb_intf_desc_t*>(desc);
        if (interface->bInterfaceClass == USB_CLASS_MASS_STORAGE &&
            interface->bInterfaceSubClass == kSubclassScsi &&
            interface->bInterfaceProtocol == kProtocolBot) {
            return interface;
        }
    }
    return nullptr;
}

MscBotDevice* blockdev_owner(esp_blockdev_handle_t handle) {
    return static_cast<MscBotDevice*>(handle->ctx);
}

esp_err_t blockdev_read(esp_blockdev_handle_t handle, uint8_t* dst, size_t dst_size,
                        uint64_t addr, size_t len) {
    MscBotDevice* device = blockdev_owner(handle);
    const uint32_t block_size = device->block_size();
    if (addr % block_size || len % block_size || dst_size < len) return ESP_ERR_INVALID_ARG;
    return device->read(dst, static_cast<uint32_t>(addr / block_size), len / block_size);
}

esp_err_t blockdev_write(esp_blockdev_handle_t handle, const uint8_t* src, uint64_t addr,
                         size_t len) {
    MscBotDevice* device = blockdev_owner(handle);
    const uint32_t block_size = device->block_size();
    if (addr % block_size || len % block_size) return ESP_ERR_INVALID_ARG;
    return device->write(src, static_cast<uint32_t>(addr / block_size), len / block_size);
}

esp_err_t blockdev_erase(esp_blockdev_handle_t, uint64_t, size_t) {
    return ESP_ERR_NOT_SUPPORTED;
}

esp_err_t blockdev_release(esp_blockdev_handle_t) {
    return ESP_OK;
}

const esp_blockdev_ops_t kBlockdevOps = {
    .read = blockdev_read,
    .write = blockdev_write,
    .erase = blockdev_erase,
    .sync = nullptr,
    .ioctl = nullptr,
    .release = blockdev_release,
};

}  // namespace

esp_err_t MscBotDevice::open(usb_host_client_handle_t client, uint8_t address,
                             std::shared_ptr<MscBotDevice>* out) {
    auto* raw = new (std::nothrow) MscBotDevice();
    if (!raw) return ESP_ERR_NO_MEM;
    std::shared_ptr<MscBotDevice> device(raw);
    const esp_err_t err = device->setup(client, address);
    if (err != ESP_OK) return err;
    ESP_LOGI(TAG, "drive at %u: %u blocks of %u bytes, bulk in %02x/%u out %02x/%u", address,
             static_cast<unsigned>(device->block_count_),
             static_cast<unsigned>(device->block_size_), device->bulk_in_,
             device->bulk_in_mps_, device->bulk_out_, device->bulk_out_mps_);
    *out = std::move(device);
    return ESP_OK;
}

MscBotDevice::~MscBotDevice() {
    if (claimed_) usb_host_interface_release(xfer_.client(), xfer_.device(), interface_);
    if (data_) {
        TransferContext::set_buffer(data_, bounce_, bounce_bytes_);
        usb_host_transfer_free(data_);
    }
    if (cmd_) usb_host_transfer_free(cmd_);
    if (xfer_.device()) close_device(xfer_.device());
}

esp_err_t MscBotDevice::setup(usb_host_client_handle_t client, uint8_t address) {
    bounce_bytes_ = kBounceBytes;
    esp_err_t err = xfer_.init(client);
    if (err != ESP_OK) return err;

    usb_device_handle_t handle = nullptr;
    err = open_device(address, &handle);
    if (err != ESP_OK) return err;
    xfer_.set_device(handle);

    const usb_config_desc_t* config = nullptr;
    err = usb_host_get_active_config_descriptor(handle, &config);
    if (err != ESP_OK) return err;

    int offset = 0;
    const usb_intf_desc_t* interface = find_interface(config, &offset);
    if (!interface) return ESP_ERR_NOT_SUPPORTED;
    interface_ = interface->bInterfaceNumber;
    err = find_endpoints(config, interface, offset);
    if (err != ESP_OK) return err;

    err = usb_host_transfer_alloc(kCommandBytes, 0, &cmd_);
    if (err != ESP_OK) return err;
    err = usb_host_transfer_alloc(bounce_bytes_, 0, &data_);
    if (err != ESP_OK) return err;
    bounce_ = data_->data_buffer;
    err = usb_host_interface_claim(client, handle, interface_, 0);
    if (err != ESP_OK) return err;
    claimed_ = true;

    err = scsi_inquiry();
    if (err == ESP_OK) err = wait_until_ready();
    if (err == ESP_OK) err = scsi_read_capacity();
    if (err != ESP_OK) return err;
    if (block_size_ < 512 || block_size_ > kBounceBytes || (block_size_ & (block_size_ - 1))) {
        ESP_LOGE(TAG, "unsupported block size %u", static_cast<unsigned>(block_size_));
        return ESP_ERR_NOT_SUPPORTED;
    }
    init_blockdev();
    return ESP_OK;
}

void MscBotDevice::init_blockdev() {
    blockdev_.ctx = this;
    blockdev_.geometry.disk_size = static_cast<uint64_t>(block_count_) * block_size_;
    blockdev_.geometry.read_size = block_size_;
    blockdev_.geometry.write_size = block_size_;
    blockdev_.geometry.erase_size = block_size_;
    blockdev_.ops = &kBlockdevOps;
}

esp_err_t MscBotDevice::control_transfer(uint8_t request_type, uint8_t request, uint16_t value,
                                         uint16_t index, uint16_t length) {
    return xfer_.control(cmd_, request_type, request, value, index, length, kTransferTimeoutMs);
}

esp_err_t MscBotDevice::clear_halt(uint8_t endpoint) {
    return xfer_.clear_halt(cmd_, endpoint, kTransferTimeoutMs);
}

esp_err_t MscBotDevice::bulk_transfer(usb_transfer_t* transfer, uint8_t endpoint, size_t bytes) {
    return xfer_.submit(transfer, endpoint, bytes, kTransferTimeoutMs);
}

esp_err_t MscBotDevice::mass_storage_reset() {
    const esp_err_t err = control_transfer(
        USB_BM_REQUEST_TYPE_DIR_OUT | USB_BM_REQUEST_TYPE_TYPE_CLASS |
            USB_BM_REQUEST_TYPE_RECIP_INTERFACE,
        kRequestReset, 0, interface_, 0);
    clear_halt(bulk_in_);
    clear_halt(bulk_out_);
    return err;
}

esp_err_t MscBotDevice::data_stage(void* data, size_t bytes, bool in) {
    const uint8_t endpoint = in ? bulk_in_ : bulk_out_;
    const bool borrowed = xfer_.borrowable(data, bytes, bulk_in_mps_);
    if (borrowed) {
        TransferContext::set_buffer(data_, data, bytes);
        TransferContext::sync_for_device(data, bytes);
    } else {
        TransferContext::set_buffer(data_, bounce_, bounce_bytes_);
        if (!in) memcpy(bounce_, data, bytes);
    }
    const size_t submit_bytes =
        in ? static_cast<size_t>(usb_round_up_to_mps(static_cast<int>(bytes), bulk_in_mps_))
           : bytes;
    const esp_err_t err = bulk_transfer(data_, endpoint, submit_bytes);
    if (borrowed && in) TransferContext::sync_for_cpu(data, bytes);
    if (err == ESP_OK && in && !borrowed) memcpy(data, bounce_, bytes);
    TransferContext::set_buffer(data_, bounce_, bounce_bytes_);
    return err;
}

esp_err_t MscBotDevice::run_command(const uint8_t* cb, uint8_t cb_length, void* data,
                                    size_t bytes, bool in) {
    auto* cbw = reinterpret_cast<Cbw*>(cmd_->data_buffer);
    memset(cbw, 0, sizeof(*cbw));
    cbw->signature = kCbwSignature;
    cbw->tag = ++tag_;
    cbw->data_length = static_cast<uint32_t>(bytes);
    cbw->flags = in ? kCbwFlagDirIn : 0;
    cbw->cb_length = cb_length;
    memcpy(cbw->cb, cb, cb_length);
    const uint32_t tag = cbw->tag;

    esp_err_t err = bulk_transfer(cmd_, bulk_out_, kCbwBytes);
    if (err != ESP_OK) {
        if (err == ESP_ERR_INVALID_RESPONSE) mass_storage_reset();
        return err;
    }

    if (bytes) {
        err = data_stage(data, bytes, in);
        if (err == ESP_ERR_INVALID_RESPONSE) {
            clear_halt(in ? bulk_in_ : bulk_out_);
        } else if (err != ESP_OK) {
            mass_storage_reset();
            return err;
        }
    }

    const size_t csw_bytes =
        static_cast<size_t>(usb_round_up_to_mps(static_cast<int>(kCswBytes), bulk_in_mps_));
    esp_err_t status = bulk_transfer(cmd_, bulk_in_, csw_bytes);
    if (status == ESP_ERR_INVALID_RESPONSE) {
        clear_halt(bulk_in_);
        status = bulk_transfer(cmd_, bulk_in_, csw_bytes);
    }
    if (status != ESP_OK) {
        mass_storage_reset();
        return status;
    }

    const auto* csw = reinterpret_cast<const Csw*>(cmd_->data_buffer);
    if (csw->signature != kCswSignature || csw->tag != tag) {
        mass_storage_reset();
        return ESP_ERR_INVALID_RESPONSE;
    }
    if (csw->status != 0 || csw->residue != 0) return ESP_FAIL;
    return err;
}

esp_err_t MscBotDevice::scsi_request_sense(uint8_t* sense_key) {
    uint8_t response[18] = {};
    const uint8_t cb[6] = {kScsiRequestSense, 0, 0, 0, sizeof(response), 0};
    const esp_err_t err = run_command(cb, sizeof(cb), response, sizeof(response), true);
    if (sense_key) *sense_key = response[2] & 0x0F;
    return err;
}

esp_err_t MscBotDevice::scsi_test_unit_ready() {
    const uint8_t cb[6] = {kScsiTestUnitReady, 0, 0, 0, 0, 0};
    return run_command(cb, sizeof(cb), nullptr, 0, true);
}

esp_err_t MscBotDevice::scsi_inquiry() {
    uint8_t response[36] = {};
    const uint8_t cb[6] = {kScsiInquiry, 0, 0, 0, sizeof(response), 0};
    return run_command(cb, sizeof(cb), response, sizeof(response), true);
}

esp_err_t MscBotDevice::scsi_read_capacity() {
    uint8_t response[8] = {};
    const uint8_t cb[10] = {kScsiReadCapacity10, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    const esp_err_t err = run_command(cb, sizeof(cb), response, sizeof(response), true);
    if (err != ESP_OK) return err;
    block_count_ = be32(response) + 1;
    block_size_ = be32(response + 4);
    return ESP_OK;
}

esp_err_t MscBotDevice::wait_until_ready() {
    const TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(kReadyTimeoutMs);
    while (true) {
        if (scsi_test_unit_ready() == ESP_OK) return ESP_OK;
        uint8_t sense_key = 0;
        const esp_err_t err = scsi_request_sense(&sense_key);
        if (err != ESP_OK) return err;
        if (sense_key != kSenseNotReady && sense_key != kSenseUnitAttention &&
            sense_key != kSenseNoSense) {
            return ESP_ERR_INVALID_STATE;
        }
        if (static_cast<int32_t>(xTaskGetTickCount() - deadline) >= 0) return ESP_ERR_TIMEOUT;
        vTaskDelay(pdMS_TO_TICKS(100));
    }
}

esp_err_t MscBotDevice::transfer_blocks(void* buffer, uint32_t block, uint32_t count,
                                        bool read) {
    uint8_t cb[10] = {};
    cb[0] = read ? kScsiRead10 : kScsiWrite10;
    cb[2] = static_cast<uint8_t>(block >> 24);
    cb[3] = static_cast<uint8_t>(block >> 16);
    cb[4] = static_cast<uint8_t>(block >> 8);
    cb[5] = static_cast<uint8_t>(block);
    cb[7] = static_cast<uint8_t>(count >> 8);
    cb[8] = static_cast<uint8_t>(count);
    return run_command(cb, sizeof(cb), buffer, static_cast<size_t>(count) * block_size_, read);
}

esp_err_t MscBotDevice::transfer_chunked(void* buffer, uint32_t block, uint32_t count,
                                         bool read) {
    if (!buffer || !count) return ESP_ERR_INVALID_ARG;
    if (block > block_count_ || count > block_count_ - block) return ESP_ERR_INVALID_SIZE;
    const uint32_t bounce_blocks = static_cast<uint32_t>(bounce_bytes_ / block_size_);
    auto* cursor = static_cast<uint8_t*>(buffer);
    esp_err_t err = ESP_OK;

    std::lock_guard<std::mutex> guard(lock_);
    if (gone_) return ESP_ERR_NOT_FOUND;
    while (count && err == ESP_OK) {
        uint32_t blocks = count > kMaxBlocksPerCommand ? kMaxBlocksPerCommand : count;
        if (!xfer_.borrowable(cursor, static_cast<size_t>(blocks) * block_size_,
                              bulk_in_mps_) &&
            blocks > bounce_blocks) {
            blocks = bounce_blocks;
        }
        err = transfer_blocks(cursor, block, blocks, read);
        cursor += static_cast<size_t>(blocks) * block_size_;
        block += blocks;
        count -= blocks;
    }
    return err;
}

esp_err_t MscBotDevice::find_endpoints(const usb_config_desc_t* config,
                                       const usb_intf_desc_t* interface, int offset) {
    auto* desc = reinterpret_cast<const usb_standard_desc_t*>(interface);
    for (int i = 0; i < interface->bNumEndpoints; i++) {
        desc = usb_parse_next_descriptor_of_type(desc, config->wTotalLength,
                                                 USB_B_DESCRIPTOR_TYPE_ENDPOINT, &offset);
        if (!desc) break;
        auto* endpoint = reinterpret_cast<const usb_ep_desc_t*>(desc);
        if (USB_EP_DESC_GET_XFERTYPE(endpoint) != USB_TRANSFER_TYPE_BULK) continue;
        if (USB_EP_DESC_GET_EP_DIR(endpoint)) {
            bulk_in_ = endpoint->bEndpointAddress;
            bulk_in_mps_ = USB_EP_DESC_GET_MPS(endpoint);
        } else {
            bulk_out_ = endpoint->bEndpointAddress;
            bulk_out_mps_ = USB_EP_DESC_GET_MPS(endpoint);
        }
    }
    return bulk_in_ && bulk_out_ ? ESP_OK : ESP_ERR_NOT_SUPPORTED;
}

esp_err_t MscBotDevice::read(void* dst, uint32_t block, uint32_t count) {
    return transfer_chunked(dst, block, count, true);
}

esp_err_t MscBotDevice::write(const void* src, uint32_t block, uint32_t count) {
    return transfer_chunked(const_cast<void*>(src), block, count, false);
}

}  // namespace usb_host::detail
