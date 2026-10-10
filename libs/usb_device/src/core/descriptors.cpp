/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "descriptors.hpp"

#include <algorithm>

#include "esp_log.h"
#include "usb_types.hpp"

namespace usb_device {

namespace {

const char* TAG = "usb_device";

constexpr uint16_t kBcdUsb = 0x0200;
constexpr uint16_t kBcdUsbBos = 0x0210;
constexpr uint8_t kCapabilityUsb20Extension = 0x02;
constexpr uint8_t kCapabilityPlatform = 0x05;
constexpr uint8_t kMsOs20PlatformUuid[16] = {0xdf, 0x60, 0xdd, 0xd8, 0x89, 0x45, 0xc7, 0x4c,
                                             0x9c, 0xd2, 0x65, 0x9d, 0x9e, 0x64, 0x8a, 0x9f};
constexpr uint32_t kMsOs20WindowsVersion = 0x06030000;
constexpr uint16_t kMsOs20SetHeader = 0x00;
constexpr uint16_t kMsOs20ConfigSubset = 0x01;
constexpr uint16_t kMsOs20FunctionSubset = 0x02;
constexpr uint16_t kMsOs20CompatibleId = 0x03;
constexpr uint16_t kMsOs20RegistryProperty = 0x04;
constexpr uint16_t kRegMultiSz = 7;
constexpr std::string_view kDeviceInterfaceGuids = "DeviceInterfaceGUIDs";
constexpr uint8_t kMaxEndpointNumber = 15;
constexpr size_t kInterfaceDescBytes = 9;
constexpr size_t kConfigDescBytes = 9;
constexpr size_t kIadBytes = 8;
constexpr size_t kStringDescMaxBytes = 254;
constexpr uint8_t kClassMisc = 0xef;
constexpr uint8_t kSubclassCommon = 0x02;
constexpr uint8_t kProtocolIad = 0x01;
constexpr uint8_t kConfigAttrReserved = 0x80;
constexpr uint8_t kConfigAttrSelfPowered = 0x40;
constexpr uint16_t kLangIdEnglishUs = 0x0409;

void put16(std::vector<uint8_t>& bytes, uint16_t value) {
    bytes.push_back(static_cast<uint8_t>(value));
    bytes.push_back(static_cast<uint8_t>(value >> 8));
}

void put32(std::vector<uint8_t>& bytes, uint32_t value) {
    put16(bytes, static_cast<uint16_t>(value));
    put16(bytes, static_cast<uint16_t>(value >> 16));
}

void set16(std::vector<uint8_t>& bytes, size_t offset, size_t value) {
    bytes[offset] = static_cast<uint8_t>(value);
    bytes[offset + 1] = static_cast<uint8_t>(value >> 8);
}

void put_utf16z(std::vector<uint8_t>& bytes, std::string_view ascii) {
    for (char c : ascii) put16(bytes, static_cast<uint8_t>(c));
    put16(bytes, 0);
}

void put_interface(detail::ConfigState& state, uint8_t number, uint8_t alternate, uint8_t cls,
                   uint8_t subclass, uint8_t protocol, uint8_t name) {
    state.interface_offset = state.bytes.size();
    state.interface = number;
    state.alternate = alternate;
    state.bytes.insert(state.bytes.end(), {static_cast<uint8_t>(kInterfaceDescBytes),
                                           detail::kDescInterface, number, alternate, 0, cls,
                                           subclass, protocol, name});
}

uint8_t intern(std::vector<std::string>& strings, std::string_view text) {
    if (text.empty()) return 0;
    auto it = std::find(strings.begin(), strings.end(), text);
    if (it != strings.end()) return static_cast<uint8_t>(it - strings.begin() + 1);
    if (strings.size() >= 255) return 0;
    strings.emplace_back(text);
    return static_cast<uint8_t>(strings.size());
}

uint32_t next_code_point(const std::string& text, size_t* pos) {
    const auto lead = static_cast<uint8_t>(text[(*pos)++]);
    int extra = 0;
    uint32_t cp = 0;
    if (lead < 0x80) return lead;
    if ((lead & 0xe0) == 0xc0) {
        extra = 1;
        cp = lead & 0x1f;
    } else if ((lead & 0xf0) == 0xe0) {
        extra = 2;
        cp = lead & 0x0f;
    } else if ((lead & 0xf8) == 0xf0) {
        extra = 3;
        cp = lead & 0x07;
    } else {
        return 0xfffd;
    }
    for (int i = 0; i < extra; i++) {
        if (*pos >= text.size() || (static_cast<uint8_t>(text[*pos]) & 0xc0) != 0x80) {
            return 0xfffd;
        }
        cp = (cp << 6) | (static_cast<uint8_t>(text[(*pos)++]) & 0x3f);
    }
    return cp;
}

const uint8_t* first_interface(const std::vector<uint8_t>& bytes, size_t from) {
    for (size_t pos = from; pos + 1 < bytes.size() && bytes[pos] > 0; pos += bytes[pos]) {
        if (bytes[pos + 1] == detail::kDescInterface) return &bytes[pos];
    }
    return nullptr;
}

esp_err_t build_config(const DeviceInfo& info, Speed speed,
                       const std::vector<std::shared_ptr<Function>>& functions,
                       detail::Descriptors* out, bool* has_iad) {
    std::vector<uint8_t>& bytes = out->config[static_cast<int>(speed)];
    bytes.assign(kConfigDescBytes, 0);
    std::vector<detail::EndpointInfo>& endpoints = out->endpoints[static_cast<int>(speed)];
    endpoints.clear();
    out->winusb.clear();
    detail::ConfigState state{speed, bytes, endpoints, out->strings, out->winusb};
    ConfigBuilder builder(state);
    const bool record_owners = out->interface_owner.empty();

    for (const auto& function : functions) {
        const size_t start = bytes.size();
        const uint8_t first = state.interfaces;
        state.function_first = first;
        function->describe(builder);
        if (state.error) return ESP_ERR_INVALID_ARG;
        const uint8_t count = state.interfaces - first;
        if (record_owners) out->interface_owner.resize(state.interfaces, function.get());
        if (count < 2) continue;

        const uint8_t* desc = first_interface(bytes, start);
        if (!desc) return ESP_ERR_INVALID_ARG;
        const uint8_t iad[kIadBytes] = {static_cast<uint8_t>(kIadBytes),
                                        detail::kDescInterfaceAssociation,
                                        first,
                                        count,
                                        desc[5],
                                        desc[6],
                                        desc[7],
                                        desc[8]};
        bytes.insert(bytes.begin() + start, iad, iad + kIadBytes);
        *has_iad = true;
    }
    if (state.interfaces != out->interface_owner.size()) {
        ESP_LOGE(TAG, "functions declared different interfaces per speed");
        return ESP_ERR_INVALID_STATE;
    }
    if (bytes.size() > UINT16_MAX) return ESP_ERR_INVALID_SIZE;

    uint8_t attributes = kConfigAttrReserved;
    if (info.self_powered) attributes |= kConfigAttrSelfPowered;
    bytes[0] = static_cast<uint8_t>(kConfigDescBytes);
    bytes[1] = detail::kDescConfig;
    bytes[2] = static_cast<uint8_t>(bytes.size());
    bytes[3] = static_cast<uint8_t>(bytes.size() >> 8);
    bytes[4] = state.interfaces;
    bytes[5] = 1;
    bytes[6] = 0;
    bytes[7] = attributes;
    bytes[8] = static_cast<uint8_t>(std::min(info.max_power_ma / 2, 250));
    return ESP_OK;
}

void build_bos(uint8_t vendor_code, size_t set_bytes, std::vector<uint8_t>* out) {
    std::vector<uint8_t>& b = *out;
    b = {5, detail::kDescBos, 0, 0, 2};
    b.insert(b.end(), {7, detail::kDescDeviceCapability, kCapabilityUsb20Extension});
    put32(b, 0);
    b.insert(b.end(), {28, detail::kDescDeviceCapability, kCapabilityPlatform, 0});
    b.insert(b.end(), std::begin(kMsOs20PlatformUuid), std::end(kMsOs20PlatformUuid));
    put32(b, kMsOs20WindowsVersion);
    put16(b, static_cast<uint16_t>(set_bytes));
    b.insert(b.end(), {vendor_code, 0});
    set16(b, 2, b.size());
}

void put_winusb_features(std::vector<uint8_t>& b, const std::string& guid) {
    put16(b, 20);
    put16(b, kMsOs20CompatibleId);
    b.insert(b.end(), {'W', 'I', 'N', 'U', 'S', 'B', 0, 0});
    b.insert(b.end(), 8, 0);
    if (guid.empty()) return;

    const size_t start = b.size();
    put16(b, 0);
    put16(b, kMsOs20RegistryProperty);
    put16(b, kRegMultiSz);
    put16(b, static_cast<uint16_t>((kDeviceInterfaceGuids.size() + 1) * 2));
    put_utf16z(b, kDeviceInterfaceGuids);
    put16(b, static_cast<uint16_t>((guid.size() + 2) * 2));
    put_utf16z(b, guid);
    put16(b, 0);
    set16(b, start, b.size() - start);
}

// A composite device lists its WinUSB functions in function subsets; a device
// with a single interface takes the features at the top level.
void build_ms_os_20(const detail::Descriptors& d, std::vector<uint8_t>* out) {
    std::vector<uint8_t>& b = *out;
    b.clear();
    put16(b, 10);
    put16(b, kMsOs20SetHeader);
    put32(b, kMsOs20WindowsVersion);
    put16(b, 0);
    if (d.interface_owner.size() <= 1) {
        if (!d.winusb.empty()) put_winusb_features(b, d.winusb.front().guid);
    } else if (!d.winusb.empty()) {
        const size_t config = b.size();
        put16(b, 8);
        put16(b, kMsOs20ConfigSubset);
        b.insert(b.end(), {0, 0, 0, 0});
        for (const auto& function : d.winusb) {
            const size_t subset = b.size();
            put16(b, 8);
            put16(b, kMsOs20FunctionSubset);
            b.insert(b.end(), {function.first_interface, 0, 0, 0});
            put_winusb_features(b, function.guid);
            set16(b, subset + 6, b.size() - subset);
        }
        set16(b, config + 6, b.size() - config);
    }
    set16(b, 8, b.size());
}

}  // namespace

Speed ConfigBuilder::speed() const {
    return state_.speed;
}

uint8_t ConfigBuilder::interface(uint8_t cls, uint8_t subclass, uint8_t protocol,
                                 std::string_view name) {
    const uint8_t number = state_.interfaces++;
    put_interface(state_, number, 0, cls, subclass, protocol, string(name));
    return number;
}

void ConfigBuilder::alternate(uint8_t interface, uint8_t alternate, uint8_t cls,
                              uint8_t subclass, uint8_t protocol, std::string_view name) {
    if (interface >= state_.interfaces) {
        ESP_LOGE(TAG, "alternate of undeclared interface %u", interface);
        state_.error = true;
        return;
    }
    put_interface(state_, interface, alternate, cls, subclass, protocol, string(name));
}

uint8_t ConfigBuilder::endpoint(EndpointType type, bool in, uint16_t max_packet_bytes,
                                uint8_t interval) {
    uint8_t& next = in ? state_.next_in : state_.next_out;
    if (state_.interfaces == 0 || next > kMaxEndpointNumber) {
        ESP_LOGE(TAG, "endpoint without an interface or past %u", kMaxEndpointNumber);
        state_.error = true;
        return 0;
    }
    const uint8_t address = next++ | (in ? 0x80 : 0x00);
    std::vector<uint8_t>& bytes = state_.bytes;
    bytes.insert(bytes.end(), {7, detail::kDescEndpoint, address, static_cast<uint8_t>(type)});
    put16(bytes, max_packet_bytes);
    bytes.push_back(interval);
    bytes[state_.interface_offset + 4]++;
    state_.endpoints.push_back(
        {address, type, max_packet_bytes, state_.interface, state_.alternate});
    return address;
}

void ConfigBuilder::append(const void* data, size_t bytes) {
    const auto* begin = static_cast<const uint8_t*>(data);
    state_.bytes.insert(state_.bytes.end(), begin, begin + bytes);
}

uint8_t ConfigBuilder::string(std::string_view text) {
    return intern(state_.strings, text);
}

void ConfigBuilder::winusb(std::string_view device_interface_guid) {
    if (state_.interfaces == state_.function_first) {
        ESP_LOGE(TAG, "winusb before the function's first interface");
        state_.error = true;
        return;
    }
    state_.winusb.push_back({state_.function_first, std::string(device_interface_guid)});
}

namespace detail {

size_t Descriptors::largest() const {
    size_t bytes = std::max<size_t>(kStringDescMaxBytes, kEp0MaxPacket);
    bytes = std::max(bytes, device.size());
    bytes = std::max(bytes, qualifier.size());
    for (const auto& c : config) bytes = std::max(bytes, c.size());
    bytes = std::max(bytes, bos.size());
    bytes = std::max(bytes, ms_os_20.size());
    return bytes;
}

esp_err_t build_descriptors(const DeviceInfo& info, Port port,
                            const std::vector<std::shared_ptr<Function>>& functions,
                            Descriptors* out) {
    *out = {};
    const uint8_t manufacturer = intern(out->strings, info.manufacturer);
    const uint8_t product = intern(out->strings, info.product);
    const uint8_t serial = intern(out->strings, info.serial);

    bool has_iad = false;
    esp_err_t err = build_config(info, Speed::Full, functions, out, &has_iad);
    if (err == ESP_OK && port == Port::HighSpeed) {
        err = build_config(info, Speed::High, functions, out, &has_iad);
    }
    if (err != ESP_OK) return err;

    if (info.ms_os_20) {
        build_ms_os_20(*out, &out->ms_os_20);
        build_bos(info.ms_os_20_vendor_code, out->ms_os_20.size(), &out->bos);
    }
    const uint16_t bcd_usb = info.ms_os_20 ? kBcdUsbBos : kBcdUsb;
    const uint8_t cls = has_iad ? kClassMisc : 0;
    const uint8_t subclass = has_iad ? kSubclassCommon : 0;
    const uint8_t protocol = has_iad ? kProtocolIad : 0;

    std::vector<uint8_t>& d = out->device;
    d = {18, kDescDevice};
    put16(d, bcd_usb);
    d.insert(d.end(), {cls, subclass, protocol, kEp0MaxPacket});
    put16(d, info.vendor_id);
    put16(d, info.product_id);
    put16(d, info.bcd_device);
    d.insert(d.end(), {manufacturer, product, serial, 1});

    if (port == Port::HighSpeed) {
        std::vector<uint8_t>& q = out->qualifier;
        q = {10, kDescDeviceQualifier};
        put16(q, bcd_usb);
        q.insert(q.end(), {cls, subclass, protocol, kEp0MaxPacket, 1, 0});
    }
    return ESP_OK;
}

size_t string_descriptor(const Descriptors& descriptors, uint8_t index, uint8_t* out,
                         size_t capacity) {
    if (capacity < 4) return 0;
    if (index == 0) {
        out[0] = 4;
        out[1] = kDescString;
        out[2] = static_cast<uint8_t>(kLangIdEnglishUs);
        out[3] = static_cast<uint8_t>(kLangIdEnglishUs >> 8);
        return 4;
    }
    if (index > descriptors.strings.size()) return 0;

    const std::string& text = descriptors.strings[index - 1];
    const size_t limit = std::min(capacity, kStringDescMaxBytes);
    size_t length = 2;
    auto put_unit = [&](uint16_t unit) {
        out[length++] = static_cast<uint8_t>(unit);
        out[length++] = static_cast<uint8_t>(unit >> 8);
    };
    for (size_t pos = 0; pos < text.size();) {
        const uint32_t cp = next_code_point(text, &pos);
        if (cp >= 0x10000) {
            if (length + 4 > limit) break;
            put_unit(static_cast<uint16_t>(0xd800 | ((cp - 0x10000) >> 10)));
            put_unit(static_cast<uint16_t>(0xdc00 | ((cp - 0x10000) & 0x3ff)));
        } else {
            if (length + 2 > limit) break;
            put_unit(static_cast<uint16_t>(cp));
        }
    }
    out[0] = static_cast<uint8_t>(length);
    out[1] = kDescString;
    return length;
}

}  // namespace detail

}  // namespace usb_device
