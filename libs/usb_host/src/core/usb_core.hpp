/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

#include "esp_err.h"
#include "usb_types.hpp"

namespace usb_host::detail {

enum class TransferStatus : uint8_t {
    Completed,
    Error,
    TimedOut,
    Canceled,
    Stall,
    Overflow,
    Skipped,
    NoDevice,
};

// (Micro)frames of an isochronous endpoint's schedule shared by its queued
// transfers: a transfer of N packets takes N times the endpoint's interval,
// and transfers beyond this wait for earlier ones to finish.
int isoc_slots(Speed speed);

struct IsocPacket {
    int num_bytes = 0;
    int actual_num_bytes = 0;
    TransferStatus status = TransferStatus::Completed;
};

class Device;
class Pipe;

// A control transfer's buffer starts with its setup packet. Completion runs
// the callback on the host's event task.
struct Transfer {
    uint8_t* data_buffer = nullptr;
    size_t data_buffer_size = 0;
    int num_bytes = 0;
    int actual_num_bytes = 0;
    TransferStatus status = TransferStatus::Completed;
    Device* device = nullptr;
    uint8_t bEndpointAddress = 0;
    void (*callback)(Transfer* transfer) = nullptr;
    void* context = nullptr;
    int num_isoc_packets = 0;
    IsocPacket* isoc_packet_desc = nullptr;

    // Owned by the host stack.
    uint8_t* allocation = nullptr;
    Transfer* next = nullptr;
    Pipe* pipe = nullptr;
    int first_slot = 0;
    bool done = false;
};

// `caps` places the buffer, whose size is rounded up to the cache line.
esp_err_t transfer_alloc(size_t bytes, int isoc_packets, uint32_t caps, Transfer** out);
void transfer_free(Transfer* transfer);
// The buffer may be swapped for the caller's own; it must be cache aligned.
void transfer_set_buffer(Transfer* transfer, void* buffer, size_t bytes);
esp_err_t transfer_submit(Transfer* transfer);
esp_err_t transfer_submit_control(Transfer* transfer);

// Halt stops the endpoint and waits for it; flush completes what is queued
// as canceled; clear resumes it with DATA0.
esp_err_t endpoint_halt(Device* device, uint8_t endpoint);
esp_err_t endpoint_flush(Device* device, uint8_t endpoint);
esp_err_t endpoint_clear(Device* device, uint8_t endpoint);

// Every class driver sharing a device opens it; the last close releases it.
esp_err_t open_device(uint8_t address, Device** out);
void close_device(Device* device);

Speed device_speed(const Device* device);
const DeviceDesc* device_descriptor(const Device* device);
const ConfigDesc* config_descriptor(const Device* device);
const std::string& device_product(const Device* device);

// Claiming creates the pipes of the alternate's endpoints; it sends no
// SET_INTERFACE.
esp_err_t interface_claim(Device* device, uint8_t interface, uint8_t alternate);
void interface_release(Device* device, uint8_t interface);

}  // namespace usb_host::detail
