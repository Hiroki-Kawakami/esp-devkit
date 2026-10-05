/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "uac_descriptors.hpp"

#include <algorithm>

#include "esp_log.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host_uac";

constexpr uint8_t kSubclassControl = 0x01;
constexpr uint8_t kSubclassStreaming = 0x02;
constexpr uint8_t kProtocolUac1 = 0x00;
constexpr uint16_t kBcdAdc1 = 0x0100;

constexpr uint8_t kCsInterface = 0x24;
constexpr uint8_t kCsEndpoint = 0x25;

constexpr uint8_t kAcHeader = 0x01;
constexpr uint8_t kAcInputTerminal = 0x02;
constexpr uint8_t kAcOutputTerminal = 0x03;
constexpr uint8_t kAcMixerUnit = 0x04;
constexpr uint8_t kAcSelectorUnit = 0x05;
constexpr uint8_t kAcFeatureUnit = 0x06;
constexpr uint8_t kAcProcessingUnit = 0x07;
constexpr uint8_t kAcExtensionUnit = 0x08;

constexpr uint8_t kAsGeneral = 0x01;
constexpr uint8_t kAsFormatType = 0x02;
constexpr uint16_t kFormatPcm = 0x0001;
constexpr uint8_t kFormatTypeI = 0x01;

constexpr uint8_t kEpGeneral = 0x01;
constexpr uint8_t kEpRateControl = 0x01;

constexpr uint8_t kFeatureMute = 0x01;
constexpr uint8_t kFeatureVolume = 0x02;
constexpr int kMaxChannels = 8;

struct Entity {
    uint8_t id = 0;
    uint8_t type = 0;
    std::vector<uint8_t> sources;
    UacFeatureUnit feature;
};

struct Pending {
    bool streaming = false;
    uint8_t terminal_link = 0;
    uint16_t format_tag = 0;
    bool type_i = false;
    bool async = false;
    UacStreamAlt alt;
};

uint16_t le16(const uint8_t* bytes) {
    return static_cast<uint16_t>(bytes[0] | (bytes[1] << 8));
}

uint32_t le24(const uint8_t* bytes) {
    return bytes[0] | (bytes[1] << 8) | (static_cast<uint32_t>(bytes[2]) << 16);
}

void add_sources(Entity* entity, const uint8_t* bytes, uint8_t length, int first, int count) {
    for (int i = 0; i < count && first + i < length; i++) {
        entity->sources.push_back(bytes[first + i]);
    }
}

void parse_control(const uint8_t* bytes, uint8_t length, std::vector<Entity>* entities,
                   bool* uac1) {
    if (length < 4) return;
    Entity entity;
    entity.id = bytes[3];
    entity.type = bytes[2];
    switch (bytes[2]) {
        case kAcHeader:
            if (length >= 5 && le16(bytes + 3) == kBcdAdc1) *uac1 = true;
            return;
        case kAcInputTerminal:
            break;
        case kAcOutputTerminal:
            if (length >= 8) entity.sources.push_back(bytes[7]);
            break;
        case kAcMixerUnit:
        case kAcSelectorUnit:
            if (length >= 5) add_sources(&entity, bytes, length, 5, bytes[4]);
            break;
        case kAcFeatureUnit: {
            if (length < 7 || bytes[5] == 0) return;
            entity.sources.push_back(bytes[4]);
            entity.feature.id = entity.id;
            const int control_size = bytes[5];
            const int channels = std::min((length - 7) / control_size, kMaxChannels);
            for (int channel = 0; channel < channels; channel++) {
                const uint8_t controls = bytes[6 + channel * control_size];
                if (controls & kFeatureMute) entity.feature.mute_channels |= 1 << channel;
                if (controls & kFeatureVolume) entity.feature.volume_channels |= 1 << channel;
            }
            break;
        }
        case kAcProcessingUnit:
        case kAcExtensionUnit:
            if (length >= 7) add_sources(&entity, bytes, length, 7, bytes[6]);
            break;
        default:
            return;
    }
    entities->push_back(std::move(entity));
}

void parse_streaming(const uint8_t* bytes, uint8_t length, Pending* pending) {
    if (length < 3) return;
    if (bytes[2] == kAsGeneral && length >= 7) {
        pending->terminal_link = bytes[3];
        pending->format_tag = le16(bytes + 5);
        return;
    }
    if (bytes[2] != kAsFormatType || length < 8 || bytes[3] != kFormatTypeI) return;
    UacFormat& format = pending->alt.format;
    format.channels = bytes[4];
    format.subframe_bytes = bytes[5];
    format.bit_resolution = bytes[6];
    const int count = bytes[7];
    if (count == 0) {
        if (length < 14) return;
        format.min_rate = le24(bytes + 8);
        format.max_rate = le24(bytes + 11);
    } else {
        for (int i = 0; i < count && 8 + i * 3 + 3 <= length; i++) {
            format.rates.push_back(le24(bytes + 8 + i * 3));
        }
        if (format.rates.empty()) return;
    }
    pending->type_i = true;
}

void parse_endpoint(const EndpointDesc* endpoint, bool capture, Pending* pending) {
    if (ep_type(endpoint) != TransferType::Isochronous) return;
    if (ep_is_in(endpoint) != capture) return;
    if ((endpoint->bmAttributes & kEpUsageMask) ==
        kEpUsageFeedback) {
        return;
    }
    if (!capture &&
        (endpoint->bmAttributes & kEpSyncMask) == kEpSyncAsync) {
        pending->async = true;
        return;
    }
    pending->alt.endpoint = endpoint->bEndpointAddress;
    pending->alt.max_packet_bytes = ep_mps(endpoint);
    pending->alt.interval = endpoint->bInterval;
}

bool sane(const UacFormat& format) {
    if (format.channels == 0 || format.subframe_bytes == 0 || format.subframe_bytes > 4) return false;
    return format.bit_resolution > 0 && format.bit_resolution <= format.subframe_bytes * 8;
}

void flush(Pending* pending, UacTopology* out, uint8_t* terminal_link) {
    if (!pending->streaming) return;
    const Pending alt = std::move(*pending);
    *pending = {};
    if (alt.alt.alternate == 0 || alt.format_tag != kFormatPcm || !alt.type_i) return;
    if (alt.async) {
        ESP_LOGW(TAG, "interface %u alt %u: asynchronous endpoint not supported",
                 alt.alt.interface, alt.alt.alternate);
        return;
    }
    if (!alt.alt.endpoint || !alt.alt.interval || !sane(alt.alt.format)) return;
    if (!out->alts.empty() && out->alts.front().interface != alt.alt.interface) return;
    *terminal_link = alt.terminal_link;
    out->alts.push_back(alt.alt);
}

const Entity* find_entity(const std::vector<Entity>& entities, uint8_t id) {
    for (const Entity& entity : entities) {
        if (entity.id == id) return &entity;
    }
    return nullptr;
}

bool find_feature(const std::vector<Entity>& entities, uint8_t id, uint8_t target, int depth,
                  UacFeatureUnit* feature) {
    if (id == target) return true;
    const Entity* entity = find_entity(entities, id);
    if (!entity || depth > static_cast<int>(entities.size())) return false;
    for (uint8_t source : entity->sources) {
        if (!find_feature(entities, source, target, depth + 1, feature)) continue;
        if (entity->type == kAcFeatureUnit &&
            (entity->feature.mute_channels || entity->feature.volume_channels)) {
            *feature = entity->feature;
        }
        return true;
    }
    return false;
}

}  // namespace

esp_err_t uac_parse(const ConfigDesc* config, bool capture, UacTopology* out) {
    *out = {};
    std::vector<Entity> entities;
    Pending pending;
    bool control_found = false;
    bool in_control = false;
    bool uac1 = false;
    uint8_t terminal_link = 0;

    const StandardDesc* desc = reinterpret_cast<const StandardDesc*>(config);
    while ((desc = next_descriptor(config, desc)) != nullptr) {
        const auto* bytes = reinterpret_cast<const uint8_t*>(desc);
        const uint8_t length = desc->bLength;
        if (desc->bDescriptorType == kDescInterface) {
            flush(&pending, out, &terminal_link);
            const auto* interface = reinterpret_cast<const InterfaceDesc*>(desc);
            const bool audio = interface->bInterfaceClass == kClassAudio &&
                               interface->bInterfaceProtocol == kProtocolUac1;
            in_control = false;
            if (audio && interface->bInterfaceSubClass == kSubclassControl && !control_found) {
                control_found = true;
                in_control = true;
                out->control_interface = interface->bInterfaceNumber;
            } else if (audio && interface->bInterfaceSubClass == kSubclassStreaming) {
                pending.streaming = true;
                pending.alt.interface = interface->bInterfaceNumber;
                pending.alt.alternate = interface->bAlternateSetting;
            }
        } else if (desc->bDescriptorType == kCsInterface && in_control) {
            parse_control(bytes, length, &entities, &uac1);
        } else if (desc->bDescriptorType == kCsInterface && pending.streaming) {
            parse_streaming(bytes, length, &pending);
        } else if (desc->bDescriptorType == kDescEndpoint && pending.streaming) {
            parse_endpoint(reinterpret_cast<const EndpointDesc*>(desc), capture, &pending);
        } else if (desc->bDescriptorType == kCsEndpoint && pending.streaming && length >= 4 &&
                   bytes[2] == kEpGeneral) {
            pending.alt.rate_control = bytes[3] & kEpRateControl;
        }
    }
    flush(&pending, out, &terminal_link);

    if (!control_found || !uac1 || out->alts.empty()) {
        out->alts.clear();
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (capture) return ESP_OK;
    for (const Entity& entity : entities) {
        if (entity.type != kAcOutputTerminal) continue;
        UacFeatureUnit feature;
        if (find_feature(entities, entity.id, terminal_link, 0, &feature)) {
            out->feature = feature;
            break;
        }
    }
    return ESP_OK;
}

}  // namespace usb_host::detail
