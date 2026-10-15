/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "usb_device_hid.hpp"

#include <algorithm>
#include <cstring>

#include "esp_heap_caps.h"
#include "esp_log.h"

namespace usb_device {

namespace {

const char* TAG = "usb_hid";

constexpr uint8_t kClassHid = 0x03;
constexpr uint8_t kDescHid = 0x21;
constexpr uint8_t kDescReport = 0x22;
constexpr uint16_t kBcdHid = 0x0111;

constexpr uint8_t kRequestGetDescriptor = 0x06;
constexpr uint8_t kRequestGetReport = 0x01;
constexpr uint8_t kRequestGetIdle = 0x02;
constexpr uint8_t kRequestGetProtocol = 0x03;
constexpr uint8_t kRequestSetReport = 0x09;
constexpr uint8_t kRequestSetIdle = 0x0a;
constexpr uint8_t kRequestSetProtocol = 0x0b;
constexpr uint8_t kReportInput = 0x01;
constexpr uint8_t kReportOutput = 0x02;

constexpr uint8_t kRequestTypeMask = 0x60;
constexpr uint8_t kRequestTypeStandard = 0x00;
constexpr uint8_t kRequestTypeClass = 0x20;
constexpr uint8_t kRecipientMask = 0x1f;
constexpr uint8_t kRecipientInterface = 0x01;

constexpr uint8_t kItemReportId = 0x84;
constexpr uint8_t kItemLong = 0xfe;

constexpr uint8_t kBootKeyboardDescriptor[] = {
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01,
    0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x75, 0x08, 0x95, 0x01, 0x81, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x75, 0x01, 0x95, 0x05, 0x91, 0x02,
    0x75, 0x03, 0x95, 0x01, 0x91, 0x01,
    0x05, 0x07, 0x19, 0x00, 0x2a, 0xff, 0x00, 0x15, 0x00, 0x26, 0xff, 0x00,
    0x75, 0x08, 0x95, 0x06, 0x81, 0x00,
    0xc0,
};

constexpr uint8_t kBootMouseDescriptor[] = {
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x05, 0x81, 0x02,
    0x75, 0x03, 0x95, 0x01, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f,
    0x75, 0x08, 0x95, 0x03, 0x81, 0x06,
    0xc0, 0xc0,
};

constexpr size_t kHidDescriptorBytes = 9;

bool has_report_ids(const std::vector<uint8_t>& descriptor) {
    size_t pos = 0;
    while (pos < descriptor.size()) {
        const uint8_t prefix = descriptor[pos];
        if (prefix == kItemLong) {
            if (pos + 1 >= descriptor.size()) return false;
            pos += 3 + descriptor[pos + 1];
            continue;
        }
        if ((prefix & 0xfc) == kItemReportId) return true;
        const uint8_t size = prefix & 0x03;
        pos += 1 + (size == 3 ? 4 : size);
    }
    return false;
}

}  // namespace

Hid::Hid(Config config) : config_(std::move(config)) {
    if (config_.report_descriptor.empty()) {
        if (config_.boot == Boot::Keyboard) {
            config_.report_descriptor.assign(std::begin(kBootKeyboardDescriptor),
                                             std::end(kBootKeyboardDescriptor));
        } else if (config_.boot == Boot::Mouse) {
            config_.report_descriptor.assign(std::begin(kBootMouseDescriptor),
                                             std::end(kBootMouseDescriptor));
        }
    }
    if (config_.max_report_bytes == 0) {
        config_.max_report_bytes = config_.boot == Boot::Mouse ? sizeof(MouseReport)
                                                               : sizeof(KeyboardReport);
    }
    report_ids_ = has_report_ids(config_.report_descriptor);
    slot_bytes_ = (config_.max_report_bytes + 3u) & ~3u;
    buffer_ = static_cast<uint8_t*>(heap_caps_aligned_alloc(
        4, slot_bytes_ * kQueueReports, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
    stored_ = static_cast<uint8_t*>(heap_caps_malloc(slot_bytes_ * kStoredReports,
                                                     MALLOC_CAP_INTERNAL));
    if (!buffer_ || !stored_) ESP_LOGE(TAG, "no memory for the report queue");
    transfer_.callback = done;
    transfer_.context = this;
}

Hid::~Hid() {
    heap_caps_free(buffer_);
    heap_caps_free(stored_);
}

void Hid::hid_descriptor(uint8_t* out) const {
    const uint16_t length = static_cast<uint16_t>(config_.report_descriptor.size());
    const uint8_t hid[kHidDescriptorBytes] = {kHidDescriptorBytes,
                                              kDescHid,
                                              static_cast<uint8_t>(kBcdHid),
                                              static_cast<uint8_t>(kBcdHid >> 8),
                                              0,
                                              1,
                                              kDescReport,
                                              static_cast<uint8_t>(length),
                                              static_cast<uint8_t>(length >> 8)};
    memcpy(out, hid, sizeof(hid));
}

void Hid::describe(ConfigBuilder& config) {
    interface_ = config.interface(kClassHid, config_.boot == Boot::None ? 0 : 1,
                                  static_cast<uint8_t>(config_.boot), config_.name);
    uint8_t hid[kHidDescriptorBytes];
    hid_descriptor(hid);
    config.append(hid, sizeof(hid));
    in_address_ = config.endpoint(EndpointType::Interrupt, true, config_.max_report_bytes,
                                  config.speed() == Speed::High ? config_.interval : 1);
}

esp_err_t Hid::send(const void* report, size_t bytes) {
    if (!report || bytes == 0 || bytes > config_.max_report_bytes) return ESP_ERR_INVALID_ARG;
    if (!buffer_ || !stored_) return ESP_ERR_NO_MEM;
    const auto* data = static_cast<const uint8_t*>(report);
    const uint8_t id = report_ids_ ? data[0] : 0;
    portENTER_CRITICAL(&lock_);
    if (!active_) {
        portEXIT_CRITICAL(&lock_);
        return ESP_ERR_INVALID_STATE;
    }
    if (count_ == kQueueReports) {
        portEXIT_CRITICAL(&lock_);
        return ESP_ERR_NO_MEM;
    }
    const int index = (head_ + count_) % kQueueReports;
    memcpy(slot(index), data, bytes);
    lengths_[index] = static_cast<uint16_t>(bytes);
    count_++;

    int stored = 0;
    while (stored < stored_count_ && stored_ids_[stored] != id) stored++;
    if (stored < kStoredReports) {
        if (stored == stored_count_) stored_count_++;
        stored_ids_[stored] = id;
        stored_lengths_[stored] = static_cast<uint16_t>(bytes);
        memcpy(stored_ + stored * slot_bytes_, data, bytes);
    }

    const bool idle = !busy_;
    busy_ = true;
    portEXIT_CRITICAL(&lock_);
    if (idle) submit_head();
    return ESP_OK;
}

void Hid::submit_head() {
    portENTER_CRITICAL(&lock_);
    transfer_.buffer = slot(head_);
    transfer_.length = lengths_[head_];
    portEXIT_CRITICAL(&lock_);
    const esp_err_t err = endpoint(in_address_).submit(&transfer_);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "submit: %s", esp_err_to_name(err));
        portENTER_CRITICAL(&lock_);
        busy_ = false;
        portEXIT_CRITICAL(&lock_);
    }
}

void Hid::done(Transfer* transfer) {
    auto* self = static_cast<Hid*>(transfer->context);
    portENTER_CRITICAL(&self->lock_);
    self->busy_ = false;
    if (transfer->status != TransferStatus::Canceled && self->count_ > 0) {
        self->head_ = (self->head_ + 1) % kQueueReports;
        self->count_--;
    }
    const bool next = self->active_ && self->count_ > 0;
    if (next) self->busy_ = true;
    portEXIT_CRITICAL(&self->lock_);
    if (next) self->submit_head();
}

void Hid::set_active(bool active) {
    portENTER_CRITICAL(&lock_);
    active_ = active;
    count_ = 0;
    if (!active) stored_count_ = 0;
    portEXIT_CRITICAL(&lock_);
    if (!active) endpoint(in_address_).flush();
}

void Hid::configured() {
    idle_ = 0;
    protocol_ = 1;
    set_active(true);
    if (config_.configured) config_.configured();
}

void Hid::unconfigured() {
    set_active(false);
    if (config_.unconfigured) config_.unconfigured();
}

void Hid::suspended() {
    set_active(false);
    if (config_.suspended) config_.suspended();
}

void Hid::resumed() {
    set_active(true);
    if (config_.resumed) config_.resumed();
}

bool Hid::get_report(uint8_t id, uint8_t* data, size_t* bytes) {
    size_t length = 0;
    portENTER_CRITICAL(&lock_);
    for (int i = 0; i < stored_count_; i++) {
        if (stored_ids_[i] != id) continue;
        length = std::min<size_t>(*bytes, stored_lengths_[i]);
        memcpy(data, stored_ + i * slot_bytes_, length);
        break;
    }
    portEXIT_CRITICAL(&lock_);
    if (length == 0) {
        length = std::min<size_t>(*bytes, config_.max_report_bytes);
        memset(data, 0, length);
        if (report_ids_ && length > 0) data[0] = id;
    }
    *bytes = length;
    return true;
}

bool Hid::control(const ControlRequest& request, uint8_t* data, size_t* bytes) {
    if ((request.request_type & kRecipientMask) != kRecipientInterface ||
        (request.index & 0xff) != interface_) {
        return false;
    }
    const uint8_t type = request.request_type & kRequestTypeMask;
    if (type == kRequestTypeStandard) {
        if (request.request != kRequestGetDescriptor) return false;
        switch (request.value >> 8) {
            case kDescHid:
                *bytes = std::min(*bytes, kHidDescriptorBytes);
                hid_descriptor(data);
                return true;
            case kDescReport:
                *bytes = std::min(*bytes, config_.report_descriptor.size());
                memcpy(data, config_.report_descriptor.data(), *bytes);
                return true;
            default:
                return false;
        }
    }
    if (type != kRequestTypeClass) return false;

    const uint8_t report_type = request.value >> 8;
    const uint8_t report_id = request.value & 0xff;
    switch (request.request) {
        case kRequestGetReport:
            return report_type == kReportInput && get_report(report_id, data, bytes);
        case kRequestSetReport:
            if (report_type != kReportOutput) return false;
            if (config_.output) config_.output(report_id, data, *bytes);
            return true;
        case kRequestGetIdle:
            *bytes = std::min<size_t>(*bytes, 1);
            if (*bytes) data[0] = idle_;
            return true;
        case kRequestSetIdle:
            idle_ = request.value >> 8;
            return true;
        case kRequestGetProtocol:
            if (config_.boot == Boot::None) return false;
            *bytes = std::min<size_t>(*bytes, 1);
            if (*bytes) data[0] = protocol_;
            return true;
        case kRequestSetProtocol:
            if (config_.boot == Boot::None) return false;
            protocol_ = request.value & 0xff;
            return true;
        default:
            return false;
    }
}

}  // namespace usb_device
