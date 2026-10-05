/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <cstdint>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "hal/usb_dwc_hal.h"
#include "usb_core.hpp"

namespace usb_host::detail {

struct PipeConfig {
    TransferType type = TransferType::Control;
    uint8_t endpoint = 0;
    uint16_t max_packet_bytes = 0;
    uint8_t interval = 0;  // bInterval as the descriptor has it
    uint8_t address = 0;
    Speed speed = Speed::Full;
};

class Pipe {
public:
    TransferType type() const { return type_; }
    uint8_t endpoint() const { return endpoint_; }
    uint16_t max_packet_bytes() const { return mps_; }

private:
    friend class Hcd;
    enum class State : uint8_t { Active, Halting, Halted, Dead };

    usb_dwc_hal_chan_t chan_ = {};
    usb_dwc_hal_ep_char_t ep_char_ = {};
    bool has_chan_ = false;
    TransferType type_ = TransferType::Control;
    bool in_ = false;
    uint8_t endpoint_ = 0;
    uint16_t mps_ = 0;
    uint32_t interval_ = 1;

    usb_dwc_ll_dma_qtd_t* qtds_ = nullptr;     // what the controller is given
    usb_dwc_ll_dma_qtd_t* qtds_nc_ = nullptr;  // the same memory, uncached
    int qtd_len_ = 0;

    Transfer* pending_ = nullptr;
    Transfer* pending_tail_ = nullptr;
    Transfer* active_ = nullptr;
    Transfer* active_tail_ = nullptr;
    State state_ = State::Active;

    int ctrl_stage_ = 0;
    int ctrl_length_ = 0;
    bool ctrl_in_ = false;
    bool ctrl_skip_ = false;

    bool running_ = false;
    bool stopping_ = false;
    int next_slot_ = 0;
    int used_slots_ = 0;

    SemaphoreHandle_t halted_ = nullptr;
};

enum class PortEvent : uint8_t { None, Connected, Disconnected, Error };

// Port events and finished transfers are handed to the host's event task,
// which `notify` wakes.
class Hcd {
public:
    esp_err_t init(int intr_flags, TaskHandle_t notify);
    void power_on();
    // Blocks for the reset and its recovery; the port is enabled after it.
    esp_err_t reset();
    bool debounce();
    Speed speed() const { return speed_; }
    // After a disconnection: drops every channel and powers the port again.
    void recover();

    PortEvent take_port_event();
    Transfer* take_done();

    esp_err_t pipe_alloc(const PipeConfig& config, Pipe** out);
    void pipe_free(Pipe* pipe);
    void pipe_update(Pipe* pipe, uint8_t address, uint16_t max_packet_bytes);
    esp_err_t submit(Pipe* pipe, Transfer* transfer);
    esp_err_t halt(Pipe* pipe);
    void flush(Pipe* pipe);
    esp_err_t clear(Pipe* pipe);

private:
    static void isr(void* arg);
    void handle_port(usb_dwc_hal_port_event_t event);
    void handle_channel(Pipe* pipe, BaseType_t* woken);
    void handle_isoc(Pipe* pipe, BaseType_t* woken);

    void start_locked(Pipe* pipe);
    void start_control_locked(Pipe* pipe, Transfer* transfer);
    void start_bulk_locked(Pipe* pipe, Transfer* transfer);
    bool fill_isoc_locked(Pipe* pipe, Transfer* transfer);
    void start_isoc_locked(Pipe* pipe);
    void reap_isoc_locked(Pipe* pipe, bool all, BaseType_t* woken);
    void finish_locked(Pipe* pipe, Transfer* transfer, TransferStatus status, BaseType_t* woken);
    void cancel_all_locked(Pipe* pipe, TransferStatus status, BaseType_t* woken);
    void post_locked(Transfer* transfer, BaseType_t* woken);
    void post_port_locked(PortEvent event);

    usb_dwc_hal_context_t hal_ = {};
    uint32_t* frame_list_ = nullptr;
    usb_dwc_hal_fifo_config_t fifo_ = {};
    intr_handle_t intr_ = nullptr;
    TaskHandle_t notify_ = nullptr;
    Speed speed_ = Speed::Full;
    volatile bool enabled_ = false;
    volatile bool resetting_ = false;
    static constexpr int kPortEvents = 4;
    PortEvent port_events_[kPortEvents] = {};
    int port_head_ = 0;
    int port_count_ = 0;
    bool notify_pending_ = false;
    Transfer* done_ = nullptr;
    Transfer* done_tail_ = nullptr;
    Pipe** pipes_ = nullptr;
    int max_pipes_ = 0;
};

Hcd& hcd();

}  // namespace usb_host::detail
