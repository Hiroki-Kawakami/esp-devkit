/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "dcd.hpp"

namespace usb_device::detail {

struct Dcd::State {};

Dcd::Dcd() = default;

Dcd::~Dcd() = default;

esp_err_t Dcd::start(Port, size_t, QueueHandle_t) {
    return ESP_ERR_NOT_SUPPORTED;
}

void Dcd::stop() {}

uint8_t* Dcd::ep0_buffer() {
    return nullptr;
}

void Dcd::ep0_send(size_t, bool) {}

void Dcd::ep0_receive(size_t) {}

void Dcd::ep0_ack() {}

void Dcd::ep0_stall() {}

void Dcd::set_address(uint8_t) {}

esp_err_t Dcd::open_endpoint(uint8_t, EndpointType, uint16_t) {
    return ESP_ERR_NOT_SUPPORTED;
}

void Dcd::close_endpoint(uint8_t) {}

esp_err_t Dcd::submit(Transfer*) {
    return ESP_ERR_INVALID_STATE;
}

void Dcd::flush(uint8_t) {}

esp_err_t Dcd::set_stall(uint8_t, bool) {
    return ESP_ERR_INVALID_STATE;
}

bool Dcd::stalled(uint8_t) {
    return false;
}

Transfer* Dcd::take_done() {
    return nullptr;
}

}  // namespace usb_device::detail
