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

enum class EventType : uint8_t { Reset, Enumerated, Suspend, Resume, Setup, Stop };

struct Event {
    EventType type;
    Speed speed;
    SetupPacket setup;
};

// The device controller. Bus events and SETUP packets are posted to `events`
// as Event values; every other call is made from the device task.
class Dcd {
public:
    Dcd();
    ~Dcd();
    Dcd(const Dcd&) = delete;
    Dcd& operator=(const Dcd&) = delete;

    // `ep0_bytes` sizes the buffer of the longest control IN data stage.
    esp_err_t start(Port port, size_t ep0_bytes, QueueHandle_t events);
    void stop();

    // Replies to the last SETUP, once. A reply that a bus reset overtook is
    // dropped.
    uint8_t* ep0_buffer();
    void ep0_send(size_t bytes, bool zlp);
    void ep0_ack();
    void ep0_stall();
    // Takes effect before the status stage of the SET_ADDRESS, as the
    // controller requires.
    void set_address(uint8_t address);

private:
    struct State;
    State* state_ = nullptr;
};

}  // namespace usb_device::detail
