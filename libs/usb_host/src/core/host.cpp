/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <algorithm>
#include <cstring>
#include <mutex>
#include <new>
#include <utility>
#include <vector>

#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_intr_alloc.h"
#include "esp_log.h"
#include "esp_private/esp_cache_private.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "hcd.hpp"
#include "host_internal.hpp"

namespace usb_host {

namespace detail {

class Device {
public:
    struct Endpoint {
        uint8_t interface;
        uint8_t address;
        Pipe* pipe;
    };

    uint8_t address = 0;
    Speed speed = Speed::Full;
    DeviceDesc desc = {};
    std::vector<uint8_t> config;
    std::string product;
    Pipe* ep0 = nullptr;
    std::vector<Endpoint> endpoints;
    int users = 0;
    bool gone = false;
    std::mutex lock;
};

}  // namespace detail

namespace {

using detail::Device;
using detail::Hcd;
using detail::Pipe;
using detail::PipeConfig;
using detail::PortEvent;
using detail::SetupPacket;
using detail::Speed;
using detail::Transfer;
using detail::TransferStatus;
using detail::TransferType;

const char* TAG = "usb_host";

struct ClassDriver {
    esp_err_t (*install)();
    void (*connected)(uint8_t address);
    void (*gone)(Device* device);
};

constexpr ClassDriver kDrivers[] = {
#if CONFIG_USBH_MSC
    {detail::msc_install, detail::msc_connected, detail::msc_gone},
#endif
#if CONFIG_USBH_UAC
    {detail::uac_install, detail::uac_connected, detail::uac_gone},
#endif
#if CONFIG_USBH_UVC
    {detail::uvc_install, detail::uvc_connected, detail::uvc_gone},
#endif
};

constexpr uint32_t kControlTimeoutMs = 1000;
constexpr uint32_t kSetAddressRecoveryMs = 10;
constexpr uint32_t kRetryDelayMs = 100;
constexpr size_t kEnumBufferBytes = 4096;
constexpr int kEnumAttempts = 3;

QueueHandle_t s_port_events;
TaskHandle_t s_event_task;
Callbacks s_callbacks;
std::mutex s_devices_lock;
std::vector<Device*> s_devices;
uint8_t s_next_address = 1;

void event_task(void*) {
    while (true) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
        Hcd& hcd = detail::hcd();
        for (PortEvent event = hcd.take_port_event(); event != PortEvent::None;
             event = hcd.take_port_event()) {
            xQueueSend(s_port_events, &event, portMAX_DELAY);
        }
        while (Transfer* transfer = hcd.take_done()) {
            const bool in = transfer->pipe && (transfer->pipe->type() == TransferType::Control ||
                                               (transfer->pipe->endpoint() & detail::kEpDirIn));
            if (in && transfer->data_buffer && transfer->actual_num_bytes > 0) {
                esp_cache_msync(transfer->data_buffer, transfer->data_buffer_size,
                                ESP_CACHE_MSYNC_FLAG_DIR_M2C);
            }
            if (transfer->callback) transfer->callback(transfer);
        }
    }
}

void give(Transfer* transfer) {
    xSemaphoreGive(static_cast<SemaphoreHandle_t>(transfer->context));
}

// A control transfer on a pipe that has no Device yet, for enumeration.
esp_err_t control_sync(Pipe* pipe, Transfer* transfer, SemaphoreHandle_t done,
                       uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                       uint16_t length, int* received) {
    auto* setup = reinterpret_cast<SetupPacket*>(transfer->data_buffer);
    setup->bmRequestType = request_type;
    setup->bRequest = request;
    setup->wValue = value;
    setup->wIndex = index;
    setup->wLength = length;
    transfer->num_bytes = static_cast<int>(sizeof(SetupPacket) + length);
    transfer->callback = give;
    transfer->context = done;
    xSemaphoreTake(done, 0);
    Hcd& hcd = detail::hcd();
    esp_err_t err = hcd.submit(pipe, transfer);
    if (err != ESP_OK) return err;
    if (xSemaphoreTake(done, pdMS_TO_TICKS(kControlTimeoutMs)) != pdTRUE) {
        hcd.halt(pipe);
        hcd.flush(pipe);
        xSemaphoreTake(done, portMAX_DELAY);
        hcd.clear(pipe);
        return ESP_ERR_TIMEOUT;
    }
    if (transfer->status != TransferStatus::Completed) {
        if (transfer->status == TransferStatus::Stall) hcd.clear(pipe);
        return transfer->status == TransferStatus::NoDevice ? ESP_ERR_NOT_FOUND : ESP_FAIL;
    }
    if (received) *received = transfer->actual_num_bytes - static_cast<int>(sizeof(SetupPacket));
    return ESP_OK;
}

esp_err_t get_descriptor(Pipe* pipe, Transfer* transfer, SemaphoreHandle_t done, uint8_t type,
                         uint8_t index, uint16_t langid, uint16_t length, int* received) {
    return control_sync(pipe, transfer, done, detail::kReqDirIn, detail::kReqGetDescriptor,
                        static_cast<uint16_t>((type << 8) | index), langid, length, received);
}

std::string utf8(const uint8_t* desc, int length) {
    std::string out;
    if (length < 2 || desc[1] != detail::kDescString) return out;
    const int units = (std::min<int>(length, desc[0]) - 2) / 2;
    auto unit = [&](int i) { return static_cast<uint32_t>(desc[2 + i * 2] | (desc[3 + i * 2] << 8)); };
    for (int i = 0; i < units; i++) {
        uint32_t code = unit(i);
        if (code >= 0xd800 && code < 0xdc00 && i + 1 < units && unit(i + 1) >= 0xdc00 &&
            unit(i + 1) < 0xe000) {
            code = 0x10000 + ((code - 0xd800) << 10) + (unit(++i) - 0xdc00);
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

/* espressif/usb resets the bus a second time between the first descriptor
   read and SET_ADDRESS. Some devices then never answer at their new address,
   so the first attempt skips it; the retry keeps it for devices that need it. */
esp_err_t enumerate(bool second_reset, Transfer* transfer, SemaphoreHandle_t done,
                    Device** out) {
    Hcd& hcd = detail::hcd();
    esp_err_t err = hcd.reset();
    if (err != ESP_OK) return err;
    const Speed speed = hcd.speed();

    PipeConfig config;
    config.type = TransferType::Control;
    config.max_packet_bytes = speed == Speed::Low ? 8 : 64;
    config.speed = speed;
    Pipe* ep0 = nullptr;
    err = hcd.pipe_alloc(config, &ep0);
    if (err != ESP_OK) return err;

    const uint8_t* data = transfer->data_buffer + sizeof(SetupPacket);
    int received = 0;
    auto fail = [&](const char* stage, esp_err_t error) {
        ESP_LOGW(TAG, "enumeration: %s: %s", stage, esp_err_to_name(error));
        hcd.pipe_free(ep0);
        return error;
    };

    err = get_descriptor(ep0, transfer, done, detail::kDescDevice, 0, 0, 8, &received);
    if (err == ESP_OK && (received < 8 || data[1] != detail::kDescDevice)) err = ESP_ERR_INVALID_RESPONSE;
    if (err != ESP_OK) return fail("device descriptor (8)", err);
    const uint8_t mps0 = data[7];
    if (second_reset) {
        err = hcd.reset();
        if (err != ESP_OK) return fail("second reset", err);
    }
    hcd.pipe_update(ep0, 0, mps0);

    const uint8_t address = s_next_address;
    s_next_address = s_next_address >= 127 ? 1 : s_next_address + 1;
    err = control_sync(ep0, transfer, done, 0, detail::kReqSetAddress, address, 0, 0, nullptr);
    if (err != ESP_OK) return fail("SET_ADDRESS", err);
    vTaskDelay(pdMS_TO_TICKS(kSetAddressRecoveryMs));
    hcd.pipe_update(ep0, address, mps0);

    auto* device = new (std::nothrow) Device();
    if (!device) return fail("device", ESP_ERR_NO_MEM);
    auto fail_device = [&](const char* stage, esp_err_t error) {
        delete device;
        return fail(stage, error);
    };

    err = get_descriptor(ep0, transfer, done, detail::kDescDevice, 0, 0, sizeof(detail::DeviceDesc),
                         &received);
    if (err == ESP_OK && received < static_cast<int>(sizeof(detail::DeviceDesc))) {
        err = ESP_ERR_INVALID_RESPONSE;
    }
    if (err != ESP_OK) return fail_device("device descriptor", err);
    memcpy(&device->desc, data, sizeof(detail::DeviceDesc));

    err = get_descriptor(ep0, transfer, done, detail::kDescConfig, 0, 0, 9, &received);
    if (err == ESP_OK && received < 9) err = ESP_ERR_INVALID_RESPONSE;
    if (err != ESP_OK) return fail_device("config descriptor (9)", err);
    const uint16_t total = static_cast<uint16_t>(data[2] | (data[3] << 8));
    if (total < 9 || total > kEnumBufferBytes) return fail_device("config descriptor size", ESP_ERR_INVALID_SIZE);
    err = get_descriptor(ep0, transfer, done, detail::kDescConfig, 0, 0, total, &received);
    if (err == ESP_OK && received < total) err = ESP_ERR_INVALID_RESPONSE;
    if (err != ESP_OK) return fail_device("config descriptor", err);
    device->config.assign(data, data + total);

    if (device->desc.iProduct &&
        get_descriptor(ep0, transfer, done, detail::kDescString, 0, 0, 255, &received) == ESP_OK &&
        received >= 4) {
        const uint16_t langid = static_cast<uint16_t>(data[2] | (data[3] << 8));
        if (get_descriptor(ep0, transfer, done, detail::kDescString, device->desc.iProduct, langid,
                           255, &received) == ESP_OK) {
            device->product = utf8(data, received);
        }
    }

    const uint8_t configuration = device->config[5];
    err = control_sync(ep0, transfer, done, 0, detail::kReqSetConfiguration, configuration, 0, 0,
                       nullptr);
    if (err != ESP_OK) return fail_device("SET_CONFIGURATION", err);

    device->address = address;
    device->speed = speed;
    device->ep0 = ep0;
    *out = device;
    return ESP_OK;
}

void destroy(Device* device) {
    Hcd& hcd = detail::hcd();
    for (const Device::Endpoint& endpoint : device->endpoints) hcd.pipe_free(endpoint.pipe);
    hcd.pipe_free(device->ep0);
    delete device;
}

void connected() {
    {
        std::lock_guard<std::mutex> guard(s_devices_lock);
        for (Device* device : s_devices) {
            if (!device->gone) return;
        }
    }
    Hcd& hcd = detail::hcd();
    if (!hcd.debounce()) return;

    Transfer* transfer = nullptr;
    SemaphoreHandle_t done = xSemaphoreCreateBinary();
    if (!done || detail::transfer_alloc(kEnumBufferBytes, 0, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL,
                                        &transfer) != ESP_OK) {
        ESP_LOGE(TAG, "enumeration: no memory");
        if (done) vSemaphoreDelete(done);
        return;
    }
    Device* device = nullptr;
    esp_err_t err = ESP_FAIL;
    for (int attempt = 0; attempt < kEnumAttempts && err != ESP_OK; attempt++) {
        if (attempt > 0) vTaskDelay(pdMS_TO_TICKS(kRetryDelayMs));
        err = enumerate(attempt == 1, transfer, done, &device);
        if (err == ESP_ERR_NOT_FOUND) break;
    }
    detail::transfer_free(transfer);
    vSemaphoreDelete(done);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "enumeration failed: %s", esp_err_to_name(err));
        return;
    }

    ESP_LOGI(TAG, "device at %u: %04x:%04x \"%s\", %s speed", device->address,
             device->desc.idVendor, device->desc.idProduct, device->product.c_str(),
             device->speed == Speed::High ? "high" : device->speed == Speed::Full ? "full" : "low");
    {
        std::lock_guard<std::mutex> guard(s_devices_lock);
        s_devices.push_back(device);
    }
    for (const ClassDriver& driver : kDrivers) driver.connected(device->address);
}

void disconnected() {
    std::vector<Device*> gone;
    {
        std::lock_guard<std::mutex> guard(s_devices_lock);
        for (Device* device : s_devices) {
            if (!device->gone) {
                device->gone = true;
                gone.push_back(device);
            }
        }
    }
    for (Device* device : gone) {
        for (const ClassDriver& driver : kDrivers) driver.gone(device);
    }
    detail::hcd().recover();
    std::lock_guard<std::mutex> guard(s_devices_lock);
    for (auto it = s_devices.begin(); it != s_devices.end();) {
        if ((*it)->gone && (*it)->users == 0) {
            destroy(*it);
            it = s_devices.erase(it);
        } else {
            ++it;
        }
    }
}

void worker_task(void*) {
    PortEvent event;
    while (true) {
        xQueueReceive(s_port_events, &event, portMAX_DELAY);
        if (event == PortEvent::Connected) {
            connected();
        } else {
            disconnected();
        }
    }
}

Device* find_device(uint8_t address) {
    for (Device* device : s_devices) {
        if (device->address == address && !device->gone) return device;
    }
    return nullptr;
}

Pipe* find_pipe(Device* device, uint8_t endpoint) {
    if ((endpoint & 0x0f) == 0) return device->ep0;
    std::lock_guard<std::mutex> guard(device->lock);
    for (const Device::Endpoint& entry : device->endpoints) {
        if (entry.address == endpoint) return entry.pipe;
    }
    return nullptr;
}

}  // namespace

const Callbacks& detail::callbacks() {
    return s_callbacks;
}

const detail::StandardDesc* detail::next_descriptor(const ConfigDesc* config,
                                                    const StandardDesc* desc) {
    const auto* base = reinterpret_cast<const uint8_t*>(config);
    const auto* at = reinterpret_cast<const uint8_t*>(desc);
    if (desc->bLength == 0) return nullptr;
    at += desc->bLength;
    if (at + 2 > base + config->wTotalLength) return nullptr;
    const auto* next = reinterpret_cast<const StandardDesc*>(at);
    if (next->bLength < 2 || at + next->bLength > base + config->wTotalLength) return nullptr;
    return next;
}

esp_err_t detail::transfer_alloc(size_t bytes, int isoc_packets, uint32_t caps, Transfer** out) {
    size_t alignment = 0;
    esp_cache_get_alignment(caps, &alignment);
    alignment = std::max<size_t>(alignment, 4);
    const size_t size = (std::max<size_t>(bytes, 1) + alignment - 1) / alignment * alignment;
    auto* transfer = new (std::nothrow) Transfer();
    auto* packets = isoc_packets > 0 ? new (std::nothrow) IsocPacket[isoc_packets] : nullptr;
    auto* buffer = static_cast<uint8_t*>(heap_caps_aligned_calloc(alignment, 1, size, caps));
    if (!transfer || !buffer || (isoc_packets > 0 && !packets)) {
        delete transfer;
        delete[] packets;
        heap_caps_free(buffer);
        return ESP_ERR_NO_MEM;
    }
    transfer->allocation = buffer;
    transfer->data_buffer = buffer;
    transfer->data_buffer_size = size;
    transfer->num_isoc_packets = isoc_packets;
    transfer->isoc_packet_desc = packets;
    *out = transfer;
    return ESP_OK;
}

void detail::transfer_free(Transfer* transfer) {
    if (!transfer) return;
    heap_caps_free(transfer->allocation);
    delete[] transfer->isoc_packet_desc;
    delete transfer;
}

void detail::transfer_set_buffer(Transfer* transfer, void* buffer, size_t bytes) {
    transfer->data_buffer = static_cast<uint8_t*>(buffer);
    transfer->data_buffer_size = bytes;
}

esp_err_t detail::transfer_submit(Transfer* transfer) {
    Device* device = transfer->device;
    if (!device) return ESP_ERR_INVALID_ARG;
    if (device->gone) return ESP_ERR_NOT_FOUND;
    Pipe* pipe = find_pipe(device, transfer->bEndpointAddress);
    if (!pipe) return ESP_ERR_INVALID_ARG;
    return hcd().submit(pipe, transfer);
}

esp_err_t detail::transfer_submit_control(Transfer* transfer) {
    Device* device = transfer->device;
    if (!device) return ESP_ERR_INVALID_ARG;
    if (device->gone) return ESP_ERR_NOT_FOUND;
    transfer->bEndpointAddress = 0;
    return hcd().submit(device->ep0, transfer);
}

esp_err_t detail::endpoint_halt(Device* device, uint8_t endpoint) {
    Pipe* pipe = find_pipe(device, endpoint);
    return pipe ? hcd().halt(pipe) : ESP_ERR_INVALID_ARG;
}

esp_err_t detail::endpoint_flush(Device* device, uint8_t endpoint) {
    Pipe* pipe = find_pipe(device, endpoint);
    if (!pipe) return ESP_ERR_INVALID_ARG;
    hcd().flush(pipe);
    return ESP_OK;
}

esp_err_t detail::endpoint_clear(Device* device, uint8_t endpoint) {
    Pipe* pipe = find_pipe(device, endpoint);
    return pipe ? hcd().clear(pipe) : ESP_ERR_INVALID_ARG;
}

esp_err_t detail::open_device(uint8_t address, Device** out) {
    std::lock_guard<std::mutex> guard(s_devices_lock);
    Device* device = find_device(address);
    if (!device) return ESP_ERR_NOT_FOUND;
    device->users++;
    *out = device;
    return ESP_OK;
}

void detail::close_device(Device* device) {
    std::lock_guard<std::mutex> guard(s_devices_lock);
    if (--device->users > 0 || !device->gone) return;
    auto it = std::find(s_devices.begin(), s_devices.end(), device);
    if (it == s_devices.end()) return;
    s_devices.erase(it);
    destroy(device);
}

Speed detail::device_speed(const Device* device) {
    return device->speed;
}

const detail::DeviceDesc* detail::device_descriptor(const Device* device) {
    return &device->desc;
}

const detail::ConfigDesc* detail::config_descriptor(const Device* device) {
    return reinterpret_cast<const ConfigDesc*>(device->config.data());
}

const std::string& detail::device_product(const Device* device) {
    return device->product;
}

esp_err_t detail::interface_claim(Device* device, uint8_t interface, uint8_t alternate) {
    if (device->gone) return ESP_ERR_NOT_FOUND;
    interface_release(device, interface);
    const ConfigDesc* config = config_descriptor(device);
    bool found = false;
    std::vector<Device::Endpoint> created;
    esp_err_t err = ESP_OK;
    for (const StandardDesc* desc = reinterpret_cast<const StandardDesc*>(config);
         (desc = next_descriptor(config, desc)) != nullptr;) {
        if (desc->bDescriptorType == kDescInterface) {
            const auto* intf = reinterpret_cast<const InterfaceDesc*>(desc);
            if (found) break;
            found = intf->bInterfaceNumber == interface && intf->bAlternateSetting == alternate;
            continue;
        }
        if (!found || desc->bDescriptorType != kDescEndpoint) continue;
        const auto* ep = reinterpret_cast<const EndpointDesc*>(desc);
        if (ep_type(ep) == TransferType::Interrupt) continue;
        PipeConfig pipe_config;
        pipe_config.type = ep_type(ep);
        pipe_config.endpoint = ep->bEndpointAddress;
        pipe_config.max_packet_bytes = ep_mps(ep);
        pipe_config.interval = ep->bInterval;
        if (ep_type(ep) == TransferType::Isochronous) pipe_config.mult = ep_mult(ep);
        pipe_config.address = device->address;
        pipe_config.speed = device->speed;
        Pipe* pipe = nullptr;
        err = hcd().pipe_alloc(pipe_config, &pipe);
        if (err != ESP_OK) break;
        created.push_back({interface, ep->bEndpointAddress, pipe});
    }
    if (err == ESP_OK && !found) err = ESP_ERR_NOT_FOUND;
    if (err != ESP_OK) {
        for (const Device::Endpoint& endpoint : created) hcd().pipe_free(endpoint.pipe);
        return err;
    }
    std::lock_guard<std::mutex> guard(device->lock);
    device->endpoints.insert(device->endpoints.end(), created.begin(), created.end());
    return ESP_OK;
}

void detail::interface_release(Device* device, uint8_t interface) {
    std::vector<Pipe*> released;
    {
        std::lock_guard<std::mutex> guard(device->lock);
        for (auto it = device->endpoints.begin(); it != device->endpoints.end();) {
            if (it->interface == interface) {
                released.push_back(it->pipe);
                it = device->endpoints.erase(it);
            } else {
                ++it;
            }
        }
    }
    for (Pipe* pipe : released) hcd().pipe_free(pipe);
}

esp_err_t install(Callbacks callbacks) {
    if (s_port_events) return ESP_ERR_INVALID_STATE;
    s_callbacks = std::move(callbacks);
    s_port_events = xQueueCreate(4, sizeof(PortEvent));
    if (!s_port_events) return ESP_ERR_NO_MEM;

    for (const ClassDriver& driver : kDrivers) {
        const esp_err_t err = driver.install();
        if (err != ESP_OK) return err;
    }

    if (xTaskCreate(event_task, "usbh_event", 4096, nullptr, 10, &s_event_task) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    esp_err_t err = detail::hcd().init(ESP_INTR_FLAG_LEVEL1, s_event_task);
    if (err != ESP_OK) return err;
    if (xTaskCreate(worker_task, "usbh", 4096, nullptr, 5, nullptr) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    detail::hcd().power_on();
    return ESP_OK;
}

}  // namespace usb_host
