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

constexpr uint8_t kClassInterfaceOut = USB_BM_REQUEST_TYPE_DIR_OUT |
                                       USB_BM_REQUEST_TYPE_TYPE_CLASS |
                                       USB_BM_REQUEST_TYPE_RECIP_INTERFACE;
constexpr uint8_t kClassInterfaceIn = USB_BM_REQUEST_TYPE_DIR_IN |
                                      USB_BM_REQUEST_TYPE_TYPE_CLASS |
                                      USB_BM_REQUEST_TYPE_RECIP_INTERFACE;
constexpr uint8_t kStandardInterfaceOut = USB_BM_REQUEST_TYPE_DIR_OUT |
                                          USB_BM_REQUEST_TYPE_TYPE_STANDARD |
                                          USB_BM_REQUEST_TYPE_RECIP_INTERFACE;

constexpr uint8_t kHeaderFid = 0x01;
constexpr uint8_t kHeaderEof = 0x02;
constexpr uint8_t kHeaderErr = 0x40;

constexpr uint32_t kControlTimeoutMs = 1000;
constexpr size_t kControlBytes = 64;
constexpr size_t kProbeBytes = 48;
constexpr uint32_t kIsocTransferUs = 1000;
constexpr int kIsocDescriptorListLength = 61;
constexpr size_t kBulkTransferBytes = 32 * 1024;
constexpr int kTransfers = 4;

uint32_t le32(const uint8_t* bytes) {
    return bytes[0] | (bytes[1] << 8) | (bytes[2] << 16) | (static_cast<uint32_t>(bytes[3]) << 24);
}

void put32(uint8_t* out, uint32_t value) {
    for (int i = 0; i < 4; i++) out[i] = static_cast<uint8_t>(value >> (i * 8));
}

std::string utf8(const usb_str_desc_t* desc) {
    std::string out;
    if (!desc) return out;
    const int units = (desc->bLength - USB_STR_DESC_SIZE) / 2;
    for (int i = 0; i < units; i++) {
        uint32_t code = desc->wData[i];
        if (code >= 0xd800 && code < 0xdc00 && i + 1 < units && desc->wData[i + 1] >= 0xdc00 &&
            desc->wData[i + 1] < 0xe000) {
            code = 0x10000 + ((code - 0xd800) << 10) + (desc->wData[++i] - 0xdc00);
        }
        if (code < 0x80) {
            out += static_cast<char>(code);
        } else if (code < 0x800) {
            out += static_cast<char>(0xc0 | (code >> 6));
            out += static_cast<char>(0x80 | (code & 0x3f));
        } else if (code < 0x10000) {
            out += static_cast<char>(0xe0 | (code >> 12));
            out += static_cast<char>(0x80 | ((code >> 6) & 0x3f));
            out += static_cast<char>(0x80 | (code & 0x3f));
        } else {
            out += static_cast<char>(0xf0 | (code >> 18));
            out += static_cast<char>(0x80 | ((code >> 12) & 0x3f));
            out += static_cast<char>(0x80 | ((code >> 6) & 0x3f));
            out += static_cast<char>(0x80 | (code & 0x3f));
        }
    }
    while (!out.empty() && (out.back() == ' ' || out.back() == '\0')) out.pop_back();
    return out;
}

uint16_t probe_bytes(uint16_t uvc_version) {
    if (uvc_version < 0x0110) return 26;
    if (uvc_version < 0x0150) return 34;
    return 48;
}

}  // namespace

esp_err_t UvcCameraDevice::open(usb_host_client_handle_t client, uint8_t address,
                                std::shared_ptr<UvcCameraDevice>* out) {
    auto* raw = new (std::nothrow) UvcCameraDevice();
    if (!raw) return ESP_ERR_NO_MEM;
    std::shared_ptr<UvcCameraDevice> device(raw);
    const esp_err_t err = device->setup(client, address);
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
    if (stats_timer_) {
        esp_timer_stop(stats_timer_);
        esp_timer_delete(stats_timer_);
    }
    {
        std::lock_guard<std::mutex> guard(lock_);
        stop_locked();
    }
    if (ctrl_) usb_host_transfer_free(ctrl_);
    if (xfer_.device()) close_device(xfer_.device());
}

esp_err_t UvcCameraDevice::setup(usb_host_client_handle_t client, uint8_t address) {
    esp_err_t err = xfer_.init(client);
    if (err == ESP_OK) err = stream_.init();
    if (err != ESP_OK) return err;

    usb_device_handle_t handle = nullptr;
    err = open_device(address, &handle);
    if (err != ESP_OK) return err;
    xfer_.set_device(handle);

    const usb_config_desc_t* config = nullptr;
    err = usb_host_get_active_config_descriptor(handle, &config);
    if (err != ESP_OK) return err;
    err = uvc_parse(config, &topology_);
    if (err != ESP_OK) return err;
    for (const UvcFrameDesc& frame : topology_.frames) sizes_.push_back(frame.size);

    usb_device_info_t info = {};
    err = usb_host_device_info(handle, &info);
    if (err != ESP_OK) return err;
    speed_ = info.speed;
    name_ = utf8(info.str_desc_product);

    return usb_host_transfer_alloc(sizeof(usb_setup_packet_t) + kControlBytes, 0, &ctrl_);
}

esp_err_t UvcCameraDevice::control(uint8_t request_type, uint8_t request, uint16_t value,
                                   uint16_t index, void* data, uint16_t length) {
    const bool in = request_type & USB_BM_REQUEST_TYPE_DIR_IN;
    uint8_t* payload = ctrl_->data_buffer + sizeof(usb_setup_packet_t);
    if (!in && length) memcpy(payload, data, length);
    const esp_err_t err =
        xfer_.control(ctrl_, request_type, request, value, index, length, kControlTimeoutMs);
    if (err != ESP_OK || !in) return err;
    const int received = ctrl_->actual_num_bytes - static_cast<int>(sizeof(usb_setup_packet_t));
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
    const uint32_t period_us = (speed_ == USB_SPEED_HIGH ? 125 : 1000) * interval;
    InStreamConfig config;
    config.endpoint = chosen->endpoint;
    config.isochronous = true;
    config.max_packet_bytes = chosen->max_packet_bytes;
    config.interval = interval;
    config.packets = std::min<int>(std::max<uint32_t>(1, kIsocTransferUs / period_us),
                                   kIsocDescriptorListLength / static_cast<int>(interval));
    config.transfers = kTransfers;
    if (config.packets < 1) return ESP_ERR_NOT_SUPPORTED;

    esp_err_t err = usb_host_interface_claim(xfer_.client(), xfer_.device(),
                                             topology_.streaming_interface, chosen->alternate);
    if (err != ESP_OK) return err;
    claimed_ = true;
    err = control(kStandardInterfaceOut, USB_B_REQUEST_SET_INTERFACE, chosen->alternate,
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

    const esp_err_t err = usb_host_interface_claim(xfer_.client(), xfer_.device(),
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
    if (!stats_timer_) {
        esp_timer_create_args_t args = {};
        args.callback = log_stats;
        args.arg = this;
        args.name = "uvc_stats";
        esp_timer_create(&args, &stats_timer_);
    }
    if (stats_timer_) esp_timer_start_periodic(stats_timer_, 1000000);
    return ESP_OK;
}

void UvcCameraDevice::stop() {
    std::lock_guard<std::mutex> guard(lock_);
    stop_locked();
}

void UvcCameraDevice::stop_locked() {
    if (stats_timer_) esp_timer_stop(stats_timer_);
    stream_.stop();
    if (claimed_) {
        if (!gone_ && isochronous_) {
            control(kStandardInterfaceOut, USB_B_REQUEST_SET_INTERFACE, 0,
                    topology_.streaming_interface, nullptr, 0);
        } else if (!gone_) {
            xfer_.clear_halt(ctrl_, topology_.bulk_endpoint, kControlTimeoutMs);
        }
        usb_host_interface_release(xfer_.client(), xfer_.device(), topology_.streaming_interface);
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
        if (assembling_ && !skip_) dropped_error_++;
        if (assembling_) skip_ = true;
        return;
    }
    size_t header = 0;
    if (!payload_start(data, len, &header)) {
        bad_headers_++;
        return;
    }
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
        if (!payload_start(data, len, &header)) {
            bad_headers_++;
            return;
        }
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
    if (assembling_ && fid != fid_) {
        fid_toggles_++;
        finish_frame();
    }
    fid_ = fid;
    if (!assembling_) {
        assembling_ = true;
        skip_ = false;
        frame_ = nullptr;
        frame_bytes_ = 0;
    }
    if ((header_info_ & kHeaderErr) && !skip_) {
        dropped_error_++;
        skip_ = true;
    }
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
            dropped_no_slot_++;
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
        if (!skip_) dropped_overflow_++;
        skip_ = true;
        return;
    }
    memcpy(frame_ + frame_bytes_, data, len);
    frame_bytes_ += len;
}

void UvcCameraDevice::payload_end() {
    if (header_info_ & kHeaderEof) finish_frame();
}

void UvcCameraDevice::finish_frame() {
    if (frame_) {
        const bool jpeg = frame_bytes_ >= 2 && frame_[0] == 0xff && frame_[1] == 0xd8;
        if (!skip_ && jpeg) {
            committed_++;
            last_commit_us_ = esp_timer_get_time();
            frames_.commit(frame_bytes_);
        } else {
            if (!skip_) dropped_not_jpeg_++;
            frames_.cancel();
        }
    }
    assembling_ = false;
    skip_ = false;
    frame_ = nullptr;
    frame_bytes_ = 0;
}

void UvcCameraDevice::log_stats(void* arg) {
    auto* self = static_cast<UvcCameraDevice*>(arg);
    const InStream::Stats in = self->stream_.take_stats();
    const UvcFrameQueue::Counts slots = self->frames_.counts();
    const int64_t now = esp_timer_get_time();
    const int64_t last_done = in.last_done_us ? (now - in.last_done_us) / 1000 : -1;
    const int64_t last_commit =
        self->last_commit_us_ ? (now - self->last_commit_us_) / 1000 : -1;
    ESP_LOGI(TAG,
             "in: %u xfers (%u ok, %u skip, %u fail [err %u ovf %u stall %u, %d..%d B], %u ended) "
             "inflight %d last %lld ms ago status %u | frames: %u ok (last %lld ms ago), drop err %u ovf %u nojpeg %u "
             "noslot %u, badhdr %u fid %u | slots: free %d fill %d ready %d held %d",
             (unsigned)in.transfers, (unsigned)in.packets_ok, (unsigned)in.packets_skipped,
             (unsigned)in.packets_failed, (unsigned)in.packets_error,
             (unsigned)in.packets_overflow, (unsigned)in.packets_stall, in.failed_bytes_min,
             in.failed_bytes_max, (unsigned)in.not_resubmitted, in.inflight, last_done,
             in.last_status, (unsigned)self->committed_.exchange(0), last_commit,
             (unsigned)self->dropped_error_.exchange(0),
             (unsigned)self->dropped_overflow_.exchange(0),
             (unsigned)self->dropped_not_jpeg_.exchange(0),
             (unsigned)self->dropped_no_slot_.exchange(0), (unsigned)self->bad_headers_.exchange(0),
             (unsigned)self->fid_toggles_.exchange(0), slots.free, slots.filling, slots.ready,
             slots.held);
}

}  // namespace usb_host::detail
