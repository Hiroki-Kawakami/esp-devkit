/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstddef>
#include <cstdint>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "usb_device.hpp"
#include "usb_types.hpp"

namespace usb_device::detail {

enum class EventType : uint8_t {
    Reset,
    Enumerated,
    Suspend,
    Resume,
    Setup,
    Ep0Received,
    TransfersDone,
    Stop,
};

struct Event {
    EventType type;
    Speed speed;
    uint16_t length;
    SetupPacket setup;
};

// The device controller. Bus events, SETUP packets and control OUT data are
// posted to `events`; finished transfers are collected by take_done() after
// a TransfersDone event. EP0 and endpoint opening are driven from the device
// task, submit() from any task.
class Dcd {
public:
    Dcd();
    ~Dcd();
    Dcd(const Dcd&) = delete;
    Dcd& operator=(const Dcd&) = delete;

    // `ep0_bytes` sizes the buffer of the longest control data stage.
    esp_err_t start(Port port, size_t ep0_bytes, QueueHandle_t events);
    void stop();

    // Replies to the last SETUP, once. A reply that a bus reset overtook is
    // dropped.
    uint8_t* ep0_buffer();
    void ep0_send(size_t bytes, bool zlp);
    // Receives the OUT data stage into ep0_buffer(); Ep0Received follows.
    void ep0_receive(size_t bytes);
    void ep0_ack();
    void ep0_stall();
    // Takes effect before the status stage of the SET_ADDRESS, as the
    // controller requires.
    void set_address(uint8_t address);

    esp_err_t open_endpoint(uint8_t address, EndpointType type, uint16_t max_packet_bytes);
    // Queued transfers come back canceled.
    void close_endpoint(uint8_t address);
    esp_err_t submit(Transfer* transfer);
    void flush(uint8_t address);
    // Clearing also resets the data toggle.
    esp_err_t set_stall(uint8_t address, bool stall);
    bool stalled(uint8_t address);

    Transfer* take_done();

private:
    struct State;
    State* state_ = nullptr;
};

}  // namespace usb_device::detail
