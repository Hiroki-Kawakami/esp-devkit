/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uvc_device.hpp"

#include <algorithm>
#include <cstring>
#include <new>

#include "esp_log.h"
#include "host_internal.hpp"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_uvc";

constexpr uint8_t kSetCur = 0x01;
constexpr uint8_t kGetCur = 0x81;
constexpr uint8_t kProbeControl = 0x01;
constexpr uint8_t kCommitControl = 0x02;

constexpr uint8_t kClassInterfaceOut = 0 | kReqTypeClass |
                                       kReqRecipInterface;
constexpr uint8_t kClassInterfaceIn = kReqDirIn |
                                      kReqTypeClass |
                                      kReqRecipInterface;
constexpr uint8_t kStandardInterfaceOut = 0 | kReqTypeStandard |
                                          kReqRecipInterface;

constexpr uint8_t kHeaderFid = 0x01;
constexpr uint8_t kHeaderEof = 0x02;
constexpr uint8_t kHeaderErr = 0x40;

constexpr uint32_t kControlTimeoutMs = 1000;
constexpr size_t kControlBytes = 64;
constexpr size_t kProbeBytes = 48;
constexpr uint32_t kIsocTransferUs = 1000;
constexpr size_t kIsocBufferBytes = 64 * 1024;
constexpr size_t kBulkTransferBytes = 32 * 1024;
constexpr int kTransfers = 4;

uint32_t le32(const uint8_t* bytes) {
    return bytes[0] | (bytes[1] << 8) | (bytes[2] << 16) | (static_cast<uint32_t>(bytes[3]) << 24);
}

void put32(uint8_t* out, uint32_t value) {
    for (int i = 0; i < 4; i++) out[i] = static_cast<uint8_t>(value >> (i * 8));
}

uint16_t probe_bytes(uint16_t uvc_version) {
    if (uvc_version < 0x0110) return 26;
    if (uvc_version < 0x0150) return 34;
    return 48;
}

}  // namespace

esp_err_t UvcCameraDevice::open(uint8_t address,
                                std::shared_ptr<UvcCameraDevice>* out) {
    auto* raw = new (std::nothrow) UvcCameraDevice();
    if (!raw) return ESP_ERR_NO_MEM;
    std::shared_ptr<UvcCameraDevice> device(raw);
    const esp_err_t err = device->setup(address);
    if (err != ESP_OK) return err;
    const UvcTopology& topology = device->topology_;
    ESP_LOGI(TAG, "camera at %u: \"%s\", UVC %x.%02x, interface %u, %s", address,
             device->name_.c_str(), topology.uvc_version >> 8, topology.uvc_version & 0xff,
             topology.streaming_interface,
             topology.isoc_alts.empty() ? "bulk" : "isochronous");
    for (const UvcFrameDesc& frame : topology.frames) {
        const UvcFrameSize& size = frame.size;
        ESP_LOGI(TAG, "  MJPEG %ux%u, %u intervals (%u..%u), max %u bytes", size.width,
                 size.height, static_cast<unsigned>(size.intervals.size()),
                 static_cast<unsigned>(size.intervals.empty() ? size.min_interval
                                                              : size.intervals.front()),
                 static_cast<unsigned>(size.intervals.empty() ? size.max_interval
                                                              : size.intervals.back()),
                 static_cast<unsigned>(frame.max_frame_bytes));
    }
    for (const UvcIsocAlt& alt : topology.isoc_alts) {
        ESP_LOGI(TAG, "  alt %u: ep %02x/%u, interval %u", alt.alternate, alt.endpoint,
                 alt.max_packet_bytes, alt.interval);
    }
    *out = std::move(device);
    return ESP_OK;
}

UvcCameraDevice::~UvcCameraDevice() {
    {
        std::lock_guard<std::mutex> guard(lock_);
        stop_locked();
    }
    if (ctrl_) transfer_free(ctrl_);
    if (xfer_.device()) close_device(xfer_.device());
}

esp_err_t UvcCameraDevice::setup(uint8_t address) {
    esp_err_t err = xfer_.init();
    if (err == ESP_OK) err = stream_.init();
    if (err != ESP_OK) return err;

    Device* handle = nullptr;
    err = open_device(address, &handle);
    if (err != ESP_OK) return err;
    xfer_.set_device(handle);

    err = uvc_parse(config_descriptor(handle), &topology_);
    if (err != ESP_OK) return err;
    for (const UvcFrameDesc& frame : topology_.frames) sizes_.push_back(frame.size);

    speed_ = device_speed(handle);
    name_ = device_product(handle);
    return transfer_alloc(sizeof(SetupPacket) + kControlBytes, 0,
                          MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &ctrl_);
}

esp_err_t UvcCameraDevice::control(uint8_t request_type, uint8_t request, uint16_t value,
                                   uint16_t index, void* data, uint16_t length) {
    const bool in = request_type & kReqDirIn;
    uint8_t* payload = ctrl_->data_buffer + sizeof(SetupPacket);
    if (!in && length) memcpy(payload, data, length);
    const esp_err_t err =
        xfer_.control(ctrl_, request_type, request, value, index, length, kControlTimeoutMs);
    if (err != ESP_OK || !in) return err;
    const int received = ctrl_->actual_num_bytes - static_cast<int>(sizeof(SetupPacket));
    if (received <= 0) return ESP_ERR_INVALID_RESPONSE;
    memcpy(data, payload, std::min<int>(received, length));
    return ESP_OK;
}

esp_err_t UvcCameraDevice::negotiate(const UvcFrameDesc& frame, uint32_t interval,
                                     uint32_t* max_payload) {
    const uint16_t length = probe_bytes(topology_.uvc_version);
    const uint16_t index = topology_.streaming_interface;
    uint8_t probe[kProbeBytes] = {};
    probe[0] = 1;
    probe[2] = topology_.format_index;
    probe[3] = frame.index;
    put32(probe + 4, interval);
    esp_err_t err = control(kClassInterfaceOut, kSetCur, kProbeControl << 8, index, probe, length);
    if (err == ESP_OK) {
        err = control(kClassInterfaceIn, kGetCur, kProbeControl << 8, index, probe, length);
    }
    if (err == ESP_OK) {
        err = control(kClassInterfaceOut, kSetCur, kCommitControl << 8, index, probe, length);
    }
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "probe/commit: %s", esp_err_to_name(err));
        return err;
    }
    *max_payload = le32(probe + 22);
    ESP_LOGI(TAG, "committed %ux%u, interval %u: frames up to %u bytes, payloads up to %u",
             frame.size.width, frame.size.height, static_cast<unsigned>(le32(probe + 4)),
             static_cast<unsigned>(le32(probe + 18)), static_cast<unsigned>(*max_payload));
    return ESP_OK;
}

esp_err_t UvcCameraDevice::start_isochronous(uint32_t max_payload) {
    const UvcIsocAlt* chosen = nullptr;
    const UvcIsocAlt* largest = nullptr;
    for (const UvcIsocAlt& alt : topology_.isoc_alts) {
        if (alt.max_packet_bytes >= max_payload &&
            (!chosen || alt.max_packet_bytes < chosen->max_packet_bytes)) {
            chosen = &alt;
        }
        if (!largest || alt.max_packet_bytes > largest->max_packet_bytes) largest = &alt;
    }
    if (!chosen) {
        ESP_LOGW(TAG, "no alternate takes %u-byte payloads, using %u",
                 static_cast<unsigned>(max_payload), largest->max_packet_bytes);
        chosen = largest;
    }

    const uint32_t interval = 1u << std::min<uint8_t>(chosen->interval - 1, 15);
    const uint32_t period_us = (speed_ == Speed::High ? 125 : 1000) * interval;
    InStreamConfig config;
    config.endpoint = chosen->endpoint;
    config.isochronous = true;
    config.max_packet_bytes = chosen->max_packet_bytes;
    config.packets = std::min<int>({static_cast<int>(std::max<uint32_t>(1, kIsocTransferUs / period_us)),
                                    isoc_slots(speed_) / (kTransfers * static_cast<int>(interval)),
                                    static_cast<int>(kIsocBufferBytes / kTransfers /
                                                     chosen->max_packet_bytes)});
    config.packets = std::max(config.packets, 1);
    config.transfers = kTransfers;
    if (config.packets < 1) return ESP_ERR_NOT_SUPPORTED;

    esp_err_t err = interface_claim(xfer_.device(), topology_.streaming_interface,
                                    chosen->alternate);
    if (err != ESP_OK) return err;
    claimed_ = true;
    err = control(kStandardInterfaceOut, kReqSetInterface, chosen->alternate,
                  topology_.streaming_interface, nullptr, 0);
    if (err != ESP_OK) return err;
    ESP_LOGI(TAG, "alt %u: %d packets of %u bytes per transfer", chosen->alternate,
             config.packets, config.max_packet_bytes);
    return stream_.start(xfer_.device(), config, on_data, this);
}

esp_err_t UvcCameraDevice::start_bulk(uint32_t max_payload) {
    const size_t mps = topology_.bulk_max_packet_bytes;
    if (!mps) return ESP_ERR_NOT_SUPPORTED;
    size_t bytes = max_payload ? (max_payload + mps - 1) / mps * mps : kBulkTransferBytes;
    bytes = std::clamp(bytes, mps, kBulkTransferBytes - kBulkTransferBytes % mps);

    InStreamConfig config;
    config.endpoint = topology_.bulk_endpoint;
    config.max_packet_bytes = static_cast<uint16_t>(mps);
    config.transfer_bytes = bytes;
    config.transfers = kTransfers;

    const esp_err_t err = interface_claim(xfer_.device(),
                                                   topology_.streaming_interface, 0);
    if (err != ESP_OK) return err;
    claimed_ = true;
    return stream_.start(xfer_.device(), config, on_data, this);
}

esp_err_t UvcCameraDevice::start(uint16_t width, uint16_t height, uint32_t interval,
                                 uint8_t* const* slots, size_t count, size_t slot_bytes) {
    std::lock_guard<std::mutex> guard(lock_);
    if (gone_) return ESP_ERR_NOT_FOUND;
    if (!slots || !count || !slot_bytes) return ESP_ERR_INVALID_ARG;
    auto frame = std::find_if(topology_.frames.begin(), topology_.frames.end(),
                              [&](const UvcFrameDesc& desc) {
                                  return desc.size.width == width && desc.size.height == height;
                              });
    if (frame == topology_.frames.end() || !frame->size.supports(interval)) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    stop_locked();

    uint32_t max_payload = 0;
    esp_err_t err = negotiate(*frame, interval, &max_payload);
    if (err != ESP_OK) return err;

    frames_.reset(slots, count, slot_bytes);
    fid_ = -1;
    assembling_ = false;
    skip_ = false;
    overflowed_ = false;
    frame_ = nullptr;
    in_payload_ = false;
    max_payload_ = max_payload;
    isochronous_ = !topology_.isoc_alts.empty();
    err = isochronous_ ? start_isochronous(max_payload) : start_bulk(max_payload);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "start: %s", esp_err_to_name(err));
        stop_locked();
        return err;
    }
    return ESP_OK;
}

void UvcCameraDevice::stop() {
    std::lock_guard<std::mutex> guard(lock_);
    stop_locked();
}

void UvcCameraDevice::stop_locked() {
    stream_.stop();
    if (claimed_) {
        if (!gone_ && isochronous_) {
            control(kStandardInterfaceOut, kReqSetInterface, 0,
                    topology_.streaming_interface, nullptr, 0);
        } else if (!gone_) {
            xfer_.clear_halt(ctrl_, topology_.bulk_endpoint, kControlTimeoutMs);
        }
        interface_release(xfer_.device(), topology_.streaming_interface);
        claimed_ = false;
    }
    frames_.clear();
}

esp_err_t UvcCameraDevice::receive(UvcFrame* frame, uint32_t timeout_ms) {
    if (gone_) return ESP_ERR_NOT_FOUND;
    return frames_.receive(frame, timeout_ms);
}

void UvcCameraDevice::release(const UvcFrame& frame) {
    frames_.release(frame);
}

void UvcCameraDevice::mark_gone() {
    ESP_LOGW(TAG, "camera gone");
    gone_ = true;
    frames_.abort();
}

void UvcCameraDevice::on_data(void* context, const uint8_t* data, size_t len, size_t requested,
                              bool ok) {
    auto* self = static_cast<UvcCameraDevice*>(context);
    if (self->isochronous_) {
        self->on_isochronous(data, len, ok);
    } else {
        self->on_bulk(data, len, requested, ok);
    }
}

void UvcCameraDevice::on_isochronous(const uint8_t* data, size_t len, bool ok) {
    if (!ok) {
        if (assembling_) skip_ = true;
        return;
    }
    size_t header = 0;
    if (!payload_start(data, len, &header)) return;
    append(data + header, len - header);
    payload_end();
}

/* A bulk payload spans transfers until one comes back short or the payload
   reaches the committed maximum; only its first bytes carry a header. */
void UvcCameraDevice::on_bulk(const uint8_t* data, size_t len, size_t requested, bool ok) {
    if (!ok) {
        if (assembling_) skip_ = true;
        in_payload_ = false;
        return;
    }
    const size_t received = len;
    if (!in_payload_) {
        if (len == 0) return;
        size_t header = 0;
        if (!payload_start(data, len, &header)) return;
        data += header;
        len -= header;
        in_payload_ = true;
        payload_bytes_ = 0;
    }
    append(data, len);
    payload_bytes_ += received;
    if (received < requested || (max_payload_ && payload_bytes_ >= max_payload_)) {
        in_payload_ = false;
        payload_end();
    }
}

bool UvcCameraDevice::payload_start(const uint8_t* data, size_t len, size_t* header) {
    if (len < 2 || data[0] < 2 || data[0] > len) return false;
    *header = data[0];
    header_info_ = data[1];
    const int fid = header_info_ & kHeaderFid;
    if (assembling_ && fid != fid_) finish_frame();
    fid_ = fid;
    if (!assembling_) {
        assembling_ = true;
        skip_ = false;
        frame_ = nullptr;
        frame_bytes_ = 0;
    }
    if (header_info_ & kHeaderErr) skip_ = true;
    return true;
}

/* The slot is taken on the first byte, not at the header: devices send
   header-only payloads between frames, and taking a slot for one could
   recycle the frame that just completed. */
void UvcCameraDevice::append(const uint8_t* data, size_t len) {
    if (!assembling_ || skip_ || len == 0) return;
    if (!frame_) {
        frame_ = frames_.begin(&frame_capacity_);
        if (!frame_) {
            skip_ = true;
            return;
        }
    }
    if (frame_bytes_ + len > frame_capacity_) {
        if (!overflowed_) {
            ESP_LOGW(TAG, "frame larger than its %u-byte slot dropped",
                     static_cast<unsigned>(frame_capacity_));
        }
        overflowed_ = true;
        skip_ = true;
        return;
    }
    memcpy(frame_ + frame_bytes_, data, len);
    frame_bytes_ += len;
}

void UvcCameraDevice::payload_end() {
    if (header_info_ & kHeaderEof) finish_frame();
}

// A frame cut short keeps its SOI but loses its EOI, and the decoder waits
// for the missing data instead of failing.
void UvcCameraDevice::finish_frame() {
    if (frame_) {
        const bool jpeg = frame_bytes_ >= 2 && frame_[0] == 0xff && frame_[1] == 0xd8;
        bool eoi = false;
        for (size_t i = frame_bytes_ > 16 ? frame_bytes_ - 16 : 0; i + 1 < frame_bytes_; i++) {
            if (frame_[i] == 0xff && frame_[i + 1] == 0xd9) eoi = true;
        }
        if (!skip_ && jpeg && eoi) {
            frames_.commit(frame_bytes_);
        } else {
            frames_.cancel();
        }
    }
    assembling_ = false;
    skip_ = false;
    frame_ = nullptr;
    frame_bytes_ = 0;
}

}  // namespace usb_host::detail
