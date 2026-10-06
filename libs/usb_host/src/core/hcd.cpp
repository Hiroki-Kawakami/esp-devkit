/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "hcd.hpp"

#include <cstring>
#include <new>

#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_intr_alloc.h"
#include "esp_log.h"
#include "esp_private/esp_cache_private.h"
#include "esp_private/usb_phy.h"
#include "freertos/task.h"
#include "hal/cache_ll.h"
#include "soc/usb_periph.h"

namespace usb_host::detail {

namespace {

const char* TAG = "usb_host";

portMUX_TYPE s_lock = portMUX_INITIALIZER_UNLOCKED;
Hcd s_hcd;

constexpr int kPort = 0;
constexpr usb_hal_frame_list_len_t kFrameListLen = USB_HAL_FRAME_LIST_LEN_32;
constexpr int kCtrlQtds = 3;
constexpr int kBulkQtds = 1;
// High speed takes 256 descriptors (one per microframe, 32 ms); the HAL's
// helpers only address 64, so the channel is started here for isochronous.
constexpr int kIsocQtdsHs = 256;
constexpr int kIsocQtdsFs = 64;
// Slots between the controller's position and the first one filled on start,
// and slots always left empty so a full ring cannot catch up with itself.
constexpr int kIsocLead = 3;
constexpr int kIsocSpare = 4;
constexpr uint32_t kInitDelayMs = 30;
constexpr uint32_t kDebounceMs = 250;
constexpr uint32_t kResetHoldMs = 30;
constexpr uint32_t kResetRecoveryMs = 30;
constexpr uint32_t kHaltTimeoutMs = 200;

constexpr uint32_t kQtdActive = 1u << 31;
constexpr uint32_t kQtdIoc = 1u << 25;
constexpr uint32_t kQtdIsocLenMask = 0xfff;

usb_dwc_ll_dma_qtd_t* uncached(void* cached) {
    return reinterpret_cast<usb_dwc_ll_dma_qtd_t*>(CACHE_LL_L2MEM_NON_CACHE_ADDR(cached));
}

size_t cache_line() {
    size_t alignment = 0;
    esp_cache_get_alignment(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &alignment);
    return alignment ? alignment : 64;
}

size_t round_up(size_t value, size_t alignment) {
    return (value + alignment - 1) / alignment * alignment;
}

TransferStatus chan_error_status(usb_dwc_hal_chan_error_t error) {
    switch (error) {
        case USB_DWC_HAL_CHAN_ERROR_STALL:
            return TransferStatus::Stall;
        case USB_DWC_HAL_CHAN_ERROR_PKT_BBL:
            return TransferStatus::Overflow;
        default:
            return TransferStatus::Error;
    }
}

usb_dwc_xfer_type_t hal_type(TransferType type) {
    return static_cast<usb_dwc_xfer_type_t>(type);
}

}  // namespace

Hcd& hcd() {
    return s_hcd;
}

int isoc_slots(Speed speed) {
    return (speed == Speed::High ? kIsocQtdsHs : kIsocQtdsFs) - kIsocSpare;
}

esp_err_t Hcd::init(int intr_flags, TaskHandle_t notify) {
    notify_ = notify;

    usb_phy_config_t phy = {};
    phy.controller = USB_PHY_CTRL_OTG;
    phy.target = USB_PHY_TARGET_UTMI;
    phy.otg_mode = USB_OTG_MODE_HOST;
    phy.otg_speed = USB_PHY_SPEED_UNDEFINED;
    usb_phy_handle_t phy_handle = nullptr;
    esp_err_t err = usb_new_phy(&phy, &phy_handle);
    if (err != ESP_OK) return err;

    usb_dwc_hal_init(&hal_, kPort);
    max_pipes_ = static_cast<int>(hal_.constant_config.chan_num_total);
    hal_.channels.hdls = static_cast<usb_dwc_hal_chan_t**>(
        calloc(max_pipes_, sizeof(usb_dwc_hal_chan_t*)));
    pipes_ = static_cast<Pipe**>(calloc(max_pipes_, sizeof(Pipe*)));
    const size_t frame_bytes = round_up(kFrameListLen * sizeof(uint32_t), cache_line());
    frame_list_ = static_cast<uint32_t*>(heap_caps_aligned_calloc(
        USB_DWC_FRAME_LIST_MEM_ALIGN, 1, frame_bytes, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
    if (!hal_.channels.hdls || !pipes_ || !frame_list_) return ESP_ERR_NO_MEM;
    esp_cache_msync(frame_list_, frame_bytes, ESP_CACHE_MSYNC_FLAG_DIR_C2M);

    // The non-periodic TX FIFO takes a 512-byte bulk packet and the periodic
    // one a 640-byte isochronous OUT packet; IN gets the rest, which has to
    // keep up with three 1024-byte transactions in one microframe.
    const uint32_t depth = hal_.constant_config.hsphy_type ? 1024 : 256;
    fifo_.nptx_fifo_lines = depth / 8;
    fifo_.ptx_fifo_lines = depth * 5 / 32;
    fifo_.rx_fifo_lines = hal_.constant_config.fifo_size - fifo_.nptx_fifo_lines -
                          fifo_.ptx_fifo_lines;

    err = esp_intr_alloc(usb_dwc_info.controllers[kPort].irq,
                         intr_flags | ESP_INTR_FLAG_INTRDISABLED, isr, this, &intr_);
    if (err != ESP_OK) return err;
    esp_intr_enable(intr_);
    vTaskDelay(pdMS_TO_TICKS(kInitDelayMs));
    return ESP_OK;
}

void Hcd::power_on() {
    usb_dwc_hal_port_init(&hal_);
    usb_dwc_hal_port_toggle_power(&hal_, true);
}

bool Hcd::debounce() {
    vTaskDelay(pdMS_TO_TICKS(kDebounceMs));
    portENTER_CRITICAL(&s_lock);
    const bool connected = usb_dwc_hal_port_check_if_connected(&hal_);
    usb_dwc_hal_disable_debounce_lock(&hal_);
    portEXIT_CRITICAL(&s_lock);
    return connected;
}

esp_err_t Hcd::reset() {
    resetting_ = true;
    usb_dwc_hal_port_toggle_reset(&hal_, true);
    vTaskDelay(pdMS_TO_TICKS(kResetHoldMs));
    usb_dwc_hal_port_toggle_reset(&hal_, false);
    vTaskDelay(pdMS_TO_TICKS(kResetRecoveryMs));
    resetting_ = false;
    if (!enabled_) return ESP_ERR_INVALID_RESPONSE;

    portENTER_CRITICAL(&s_lock);
    usb_dwc_hal_set_fifo_config(&hal_, &fifo_);
    usb_dwc_hal_port_set_frame_list(&hal_, frame_list_, kFrameListLen);
    usb_dwc_hal_port_periodic_enable(&hal_);
    portEXIT_CRITICAL(&s_lock);
    return ESP_OK;
}

void Hcd::recover() {
    portENTER_CRITICAL(&s_lock);
    for (int i = 0; i < max_pipes_; i++) {
        Pipe* pipe = pipes_[i];
        if (!pipe) continue;
        cancel_all_locked(pipe, TransferStatus::NoDevice, nullptr);
        pipe->state_ = Pipe::State::Dead;
        pipe->chan_.flags.active = 0;
        pipe->has_chan_ = false;
    }
    enabled_ = false;
    portEXIT_CRITICAL(&s_lock);
    if (notify_pending_) {
        notify_pending_ = false;
        xTaskNotifyGive(notify_);
    }

    esp_intr_disable(intr_);
    usb_dwc_hal_core_soft_reset(&hal_);
    memset(frame_list_, 0, kFrameListLen * sizeof(uint32_t));
    esp_cache_msync(frame_list_, round_up(kFrameListLen * sizeof(uint32_t), cache_line()),
                    ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    esp_intr_enable(intr_);
    power_on();
}

PortEvent Hcd::take_port_event() {
    portENTER_CRITICAL(&s_lock);
    PortEvent event = PortEvent::None;
    if (port_count_ > 0) {
        event = port_events_[port_head_];
        port_head_ = (port_head_ + 1) % kPortEvents;
        port_count_--;
    }
    portEXIT_CRITICAL(&s_lock);
    return event;
}

Transfer* Hcd::take_done() {
    portENTER_CRITICAL(&s_lock);
    Transfer* transfer = done_;
    if (transfer) {
        done_ = transfer->next;
        if (!done_) done_tail_ = nullptr;
        transfer->next = nullptr;
    }
    portEXIT_CRITICAL(&s_lock);
    return transfer;
}

esp_err_t Hcd::pipe_alloc(const PipeConfig& config, Pipe** out) {
    if (config.type == TransferType::Interrupt) return ESP_ERR_NOT_SUPPORTED;
    auto* pipe = new (std::nothrow) Pipe();
    if (!pipe) return ESP_ERR_NO_MEM;
    pipe->type_ = config.type;
    pipe->endpoint_ = config.endpoint;
    pipe->in_ = config.endpoint & kEpDirIn;
    pipe->mps_ = config.max_packet_bytes;
    const int isoc_qtds = config.speed == Speed::High ? kIsocQtdsHs : kIsocQtdsFs;
    pipe->qtd_len_ = config.type == TransferType::Control  ? kCtrlQtds
                     : config.type == TransferType::Bulk ? kBulkQtds
                                                         : isoc_qtds;
    if (config.type == TransferType::Isochronous) {
        pipe->interval_ = 1u << (config.interval > 0 ? config.interval - 1 : 0);
        if (pipe->interval_ > static_cast<uint32_t>(isoc_qtds / 4)) pipe->interval_ = isoc_qtds / 4;
    }

    const size_t bytes = round_up(pipe->qtd_len_ * sizeof(usb_dwc_ll_dma_qtd_t), cache_line());
    const size_t align = bytes > USB_DWC_QTD_LIST_MEM_ALIGN ? bytes : USB_DWC_QTD_LIST_MEM_ALIGN;
    pipe->qtds_ = static_cast<usb_dwc_ll_dma_qtd_t*>(
        heap_caps_aligned_calloc(align, 1, bytes, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
    pipe->halted_ = xSemaphoreCreateBinary();
    if (!pipe->qtds_ || !pipe->halted_) {
        heap_caps_free(pipe->qtds_);
        if (pipe->halted_) vSemaphoreDelete(pipe->halted_);
        delete pipe;
        return ESP_ERR_NO_MEM;
    }
    // The CPU goes through the uncached alias from here on, so the controller
    // and the CPU never fight over a cache line holding several descriptors.
    esp_cache_msync(pipe->qtds_, bytes, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
    pipe->qtds_nc_ = uncached(pipe->qtds_);

    usb_dwc_hal_ep_char_t& ep = pipe->ep_char_;
    ep.type = hal_type(config.type);
    ep.bEndpointAddress = config.endpoint;
    ep.mps = config.max_packet_bytes;
    ep.dev_addr = config.address;
    ep.ls_via_fs_hub = 0;
    if (config.type == TransferType::Isochronous) {
        ep.periodic.interval = pipe->interval_;
        ep.periodic.offset = 0;
        ep.periodic.is_hs = config.speed == Speed::High;
    }

    esp_err_t err = ESP_OK;
    portENTER_CRITICAL(&s_lock);
    int slot = -1;
    for (int i = 0; i < max_pipes_; i++) {
        if (!pipes_[i]) {
            slot = i;
            break;
        }
    }
    if (!enabled_) {
        err = ESP_ERR_INVALID_STATE;
    } else if (slot < 0 || !usb_dwc_hal_chan_alloc(&hal_, &pipe->chan_, pipe)) {
        err = ESP_ERR_NOT_FOUND;
    } else {
        usb_dwc_hal_chan_set_ep_char(&hal_, &pipe->chan_, &ep);
        pipe->chan_.regs->hcchar_reg.ec = config.mult + 1;
        pipe->has_chan_ = true;
        pipes_[slot] = pipe;
    }
    portEXIT_CRITICAL(&s_lock);
    if (err != ESP_OK) {
        heap_caps_free(pipe->qtds_);
        vSemaphoreDelete(pipe->halted_);
        delete pipe;
        return err;
    }
    esp_cache_msync(frame_list_, round_up(kFrameListLen * sizeof(uint32_t), cache_line()),
                    ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    *out = pipe;
    return ESP_OK;
}

void Hcd::pipe_free(Pipe* pipe) {
    if (!pipe) return;
    if (pipe->state_ != Pipe::State::Dead) {
        halt(pipe);
        flush(pipe);
    }
    portENTER_CRITICAL(&s_lock);
    for (int i = 0; i < max_pipes_; i++) {
        if (pipes_[i] == pipe) pipes_[i] = nullptr;
    }
    if (pipe->has_chan_) {
        pipe->chan_.flags.active = 0;
        usb_dwc_hal_chan_free(&hal_, &pipe->chan_);
        pipe->has_chan_ = false;
    }
    portEXIT_CRITICAL(&s_lock);
    esp_cache_msync(frame_list_, round_up(kFrameListLen * sizeof(uint32_t), cache_line()),
                    ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    heap_caps_free(pipe->qtds_);
    vSemaphoreDelete(pipe->halted_);
    delete pipe;
}

void Hcd::pipe_update(Pipe* pipe, uint8_t address, uint16_t max_packet_bytes) {
    portENTER_CRITICAL(&s_lock);
    pipe->ep_char_.dev_addr = address;
    pipe->ep_char_.mps = max_packet_bytes;
    pipe->mps_ = max_packet_bytes;
    if (pipe->has_chan_ && !pipe->chan_.flags.active) {
        usb_dwc_hal_chan_set_ep_char(&hal_, &pipe->chan_, &pipe->ep_char_);
    }
    portEXIT_CRITICAL(&s_lock);
}

esp_err_t Hcd::submit(Pipe* pipe, Transfer* transfer) {
    transfer->pipe = pipe;
    transfer->next = nullptr;
    transfer->done = false;
    transfer->actual_num_bytes = 0;
    if (transfer->num_bytes > 0 && transfer->data_buffer) {
        const bool control = pipe->type_ == TransferType::Control;
        if (control || !pipe->in_) {
            esp_cache_msync(transfer->data_buffer, transfer->num_bytes,
                            ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED);
        }
        if (control || pipe->in_) {
            esp_cache_msync(transfer->data_buffer, transfer->data_buffer_size,
                            ESP_CACHE_MSYNC_FLAG_DIR_M2C);
        }
    }

    esp_err_t err = ESP_OK;
    portENTER_CRITICAL(&s_lock);
    if (pipe->state_ == Pipe::State::Dead) {
        err = ESP_ERR_NOT_FOUND;
    } else {
        if (pipe->pending_tail_) {
            pipe->pending_tail_->next = transfer;
        } else {
            pipe->pending_ = transfer;
        }
        pipe->pending_tail_ = transfer;
        if (pipe->state_ == Pipe::State::Active) start_locked(pipe);
    }
    portEXIT_CRITICAL(&s_lock);
    return err;
}

esp_err_t Hcd::halt(Pipe* pipe) {
    bool wait = false;
    portENTER_CRITICAL(&s_lock);
    if (pipe->state_ == Pipe::State::Active || pipe->state_ == Pipe::State::Halting) {
        if (!pipe->has_chan_ || !pipe->chan_.flags.active) {
            pipe->state_ = Pipe::State::Halted;
            pipe->running_ = false;
            pipe->stopping_ = false;
        } else {
            pipe->state_ = Pipe::State::Halting;
            xSemaphoreTake(pipe->halted_, 0);
            if (pipe->type_ == TransferType::Isochronous) {
                pipe->chan_.flags.halt_requested = 1;
                usb_dwc_ll_hcchar_disable_chan(pipe->chan_.regs);
            } else {
                usb_dwc_hal_chan_request_halt(&pipe->chan_);
            }
            wait = true;
        }
    }
    portEXIT_CRITICAL(&s_lock);
    if (wait && xSemaphoreTake(pipe->halted_, pdMS_TO_TICKS(kHaltTimeoutMs)) != pdTRUE) {
        ESP_LOGW(TAG, "endpoint %02x did not halt", pipe->endpoint_);
        portENTER_CRITICAL(&s_lock);
        pipe->chan_.flags.active = 0;
        pipe->chan_.flags.halt_requested = 0;
        pipe->running_ = false;
        pipe->stopping_ = false;
        if (pipe->state_ == Pipe::State::Halting) pipe->state_ = Pipe::State::Halted;
        portEXIT_CRITICAL(&s_lock);
    }
    return ESP_OK;
}

void Hcd::flush(Pipe* pipe) {
    portENTER_CRITICAL(&s_lock);
    cancel_all_locked(pipe,
                      pipe->state_ == Pipe::State::Dead ? TransferStatus::NoDevice
                                                        : TransferStatus::Canceled,
                      nullptr);
    portEXIT_CRITICAL(&s_lock);
    if (notify_pending_) {
        notify_pending_ = false;
        xTaskNotifyGive(notify_);
    }
}

esp_err_t Hcd::clear(Pipe* pipe) {
    esp_err_t err = ESP_OK;
    portENTER_CRITICAL(&s_lock);
    if (pipe->state_ == Pipe::State::Dead) {
        err = ESP_ERR_NOT_FOUND;
    } else if (pipe->state_ == Pipe::State::Halting) {
        err = ESP_ERR_INVALID_STATE;
    } else {
        pipe->state_ = Pipe::State::Active;
        if (pipe->type_ != TransferType::Isochronous && pipe->has_chan_) {
            usb_dwc_hal_chan_set_pid(&pipe->chan_, 0);
        }
        start_locked(pipe);
    }
    portEXIT_CRITICAL(&s_lock);
    return err;
}

void Hcd::start_locked(Pipe* pipe) {
    if (!pipe->has_chan_ || !enabled_) return;
    if (pipe->type_ == TransferType::Isochronous) {
        if (pipe->stopping_) return;
        if (!pipe->running_) {
            start_isoc_locked(pipe);
            return;
        }
        while (pipe->pending_ && fill_isoc_locked(pipe, pipe->pending_)) {
        }
        return;
    }
    if (pipe->active_ || !pipe->pending_) return;
    Transfer* transfer = pipe->pending_;
    pipe->pending_ = transfer->next;
    if (!pipe->pending_) pipe->pending_tail_ = nullptr;
    transfer->next = nullptr;
    pipe->active_ = transfer;
    pipe->active_tail_ = transfer;
    if (pipe->type_ == TransferType::Control) {
        start_control_locked(pipe, transfer);
    } else {
        start_bulk_locked(pipe, transfer);
    }
}

void Hcd::start_control_locked(Pipe* pipe, Transfer* transfer) {
    const auto* setup = reinterpret_cast<const SetupPacket*>(transfer->data_buffer);
    pipe->ctrl_in_ = setup->bmRequestType & kReqDirIn;
    pipe->ctrl_skip_ = setup->wLength == 0;
    pipe->ctrl_stage_ = 0;
    int length = transfer->num_bytes - static_cast<int>(sizeof(SetupPacket));
    const int capacity = static_cast<int>(transfer->data_buffer_size) -
                         static_cast<int>(sizeof(SetupPacket));
    if (pipe->ctrl_in_ && pipe->mps_) {
        const int rounded = (length + pipe->mps_ - 1) / pipe->mps_ * pipe->mps_;
        if (rounded <= capacity) length = rounded;
    }
    pipe->ctrl_length_ = length;

    usb_dwc_ll_dma_qtd_t* qtds = pipe->qtds_nc_;
    usb_dwc_ll_qtd_set_out(&qtds[0], transfer->data_buffer, sizeof(SetupPacket), true, true);
    if (pipe->ctrl_skip_) {
        usb_dwc_ll_qtd_set_null(&qtds[1]);
    } else if (pipe->ctrl_in_) {
        usb_dwc_ll_qtd_set_in(&qtds[1], transfer->data_buffer + sizeof(SetupPacket), length, true);
    } else {
        usb_dwc_ll_qtd_set_out(&qtds[1], transfer->data_buffer + sizeof(SetupPacket), length, true,
                               false);
    }
    if (pipe->ctrl_in_ && !pipe->ctrl_skip_) {
        usb_dwc_ll_qtd_set_out(&qtds[2], nullptr, 0, true, false);
    } else {
        usb_dwc_ll_qtd_set_in(&qtds[2], nullptr, 0, true);
    }
    usb_dwc_hal_chan_set_dir(&pipe->chan_, false);
    usb_dwc_hal_chan_set_pid(&pipe->chan_, 0);
    usb_dwc_hal_chan_activate(&pipe->chan_, pipe->qtds_, kCtrlQtds, 0);
}

void Hcd::start_bulk_locked(Pipe* pipe, Transfer* transfer) {
    usb_dwc_ll_dma_qtd_t* qtd = &pipe->qtds_nc_[0];
    if (pipe->in_) {
        usb_dwc_ll_qtd_set_in(qtd, transfer->data_buffer, transfer->num_bytes, true);
    } else {
        usb_dwc_ll_qtd_set_out(qtd, transfer->data_buffer, transfer->num_bytes, true, false);
    }
    usb_dwc_hal_chan_activate(&pipe->chan_, pipe->qtds_, kBulkQtds, 0);
}

bool Hcd::fill_isoc_locked(Pipe* pipe, Transfer* transfer) {
    const int packets = transfer->num_isoc_packets;
    const int needed = packets * static_cast<int>(pipe->interval_);
    if (packets < 1 || pipe->used_slots_ + needed > pipe->qtd_len_ - kIsocSpare) return false;

    if (transfer == pipe->pending_) {
        pipe->pending_ = transfer->next;
        if (!pipe->pending_) pipe->pending_tail_ = nullptr;
        transfer->next = nullptr;
    }
    transfer->first_slot = pipe->next_slot_;
    int slot = pipe->next_slot_;
    uint8_t* data = transfer->data_buffer;
    for (int i = 0; i < packets; i++) {
        const int bytes = transfer->isoc_packet_desc[i].num_bytes;
        usb_dwc_ll_dma_qtd_t* qtd = &pipe->qtds_nc_[slot];
        qtd->buffer = data;
        __asm__ __volatile__("fence" ::: "memory");
        qtd->buffer_status_val = (static_cast<uint32_t>(bytes) & kQtdIsocLenMask) |
                                 (i == packets - 1 ? kQtdIoc : 0) | kQtdActive;
        data += bytes;
        slot = (slot + static_cast<int>(pipe->interval_)) % pipe->qtd_len_;
    }
    pipe->next_slot_ = slot;
    pipe->used_slots_ += needed;
    if (pipe->active_tail_) {
        pipe->active_tail_->next = transfer;
    } else {
        pipe->active_ = transfer;
    }
    pipe->active_tail_ = transfer;
    return true;
}

void Hcd::start_isoc_locked(Pipe* pipe) {
    if (!pipe->pending_) return;
    const uint32_t interval = pipe->interval_;
    uint32_t start = usb_dwc_hal_port_get_cur_frame_num(&hal_) + 1 + kIsocLead;
    if (interval > 1) {
        const uint32_t misalign = (start - pipe->ep_char_.periodic.offset) % interval;
        if (misalign) start += interval - misalign;
    }
    pipe->next_slot_ = static_cast<int>(start % pipe->qtd_len_);
    pipe->used_slots_ = 0;
    const int first = pipe->next_slot_;
    while (pipe->pending_ && fill_isoc_locked(pipe, pipe->pending_)) {
    }
    if (!pipe->active_) return;
    pipe->running_ = true;
    volatile usb_dwc_host_chan_regs_t* regs = pipe->chan_.regs;
    usb_dwc_ll_hctsiz_set_dopng(regs, false);
    regs->hcdma_reg.val = (reinterpret_cast<uint32_t>(pipe->qtds_) &
                           ~static_cast<uint32_t>(pipe->qtd_len_ * sizeof(usb_dwc_ll_dma_qtd_t) - 1)) |
                          (static_cast<uint32_t>(first) << 3);
    usb_dwc_ll_hctsiz_set_qtd_list_len(regs, pipe->qtd_len_);
    usb_dwc_ll_hcchar_enable_chan(regs);
    pipe->chan_.flags.active = 1;
}

/* A descriptor whose (micro)frame went by unserved keeps its active bit, so
   a transfer is also done once the controller's position, counted from the
   oldest queued slot, has passed its last slot; waiting for the bit alone
   holds it, and everything queued behind it, for a whole lap of the ring. */
void Hcd::reap_isoc_locked(Pipe* pipe, bool all, BaseType_t* woken) {
    const int position = static_cast<int>((pipe->chan_.regs->hcdma_reg.val >> 3) &
                                          static_cast<uint32_t>(pipe->qtd_len_ - 1));
    const int len = pipe->qtd_len_;
    const int base = pipe->active_ ? pipe->active_->first_slot : 0;
    const int reached = (position - base + len) % len;
    while (Transfer* transfer = pipe->active_) {
        const int packets = transfer->num_isoc_packets;
        const int interval = static_cast<int>(pipe->interval_);
        const int last = (transfer->first_slot + (packets - 1) * interval) % len;
        const int end = (last - base + len) % len;
        if (!all && (pipe->qtds_nc_[last].buffer_status_val & kQtdActive) && reached <= end) break;

        int slot = transfer->first_slot;
        int total = 0;
        for (int i = 0; i < packets; i++) {
            usb_dwc_ll_dma_qtd_t* qtd = &pipe->qtds_nc_[slot];
            const uint32_t word = qtd->buffer_status_val;
            IsocPacket& packet = transfer->isoc_packet_desc[i];
            if (word & kQtdActive) {
                packet.status = TransferStatus::Skipped;
                packet.actual_num_bytes = 0;
            } else {
                const uint32_t status = (word >> 28) & 0x3;
                packet.status = status == USB_DWC_LL_QTD_STATUS_SUCCESS ? TransferStatus::Completed
                                                                        : TransferStatus::Error;
                packet.actual_num_bytes =
                    packet.num_bytes - static_cast<int>(word & kQtdIsocLenMask);
                if (packet.actual_num_bytes < 0) packet.actual_num_bytes = 0;
            }
            total += packet.actual_num_bytes;
            qtd->buffer_status_val = 0;
            qtd->buffer = nullptr;
            slot = (slot + interval) % pipe->qtd_len_;
        }
        pipe->used_slots_ -= packets * interval;
        pipe->active_ = transfer->next;
        if (!pipe->active_) pipe->active_tail_ = nullptr;
        transfer->next = nullptr;
        transfer->actual_num_bytes = total;
        transfer->status = TransferStatus::Completed;
        post_locked(transfer, woken);
    }
}

void Hcd::finish_locked(Pipe* pipe, Transfer* transfer, TransferStatus status,
                        BaseType_t* woken) {
    if (status == TransferStatus::Completed) {
        if (pipe->type_ == TransferType::Control) {
            int actual = static_cast<int>(sizeof(SetupPacket));
            if (!pipe->ctrl_skip_) {
                const int remaining = pipe->qtds_nc_[1].in_non_iso.xfer_size;
                actual += pipe->ctrl_length_ - remaining;
            }
            transfer->actual_num_bytes = actual;
        } else {
            transfer->actual_num_bytes =
                transfer->num_bytes - static_cast<int>(pipe->qtds_nc_[0].in_non_iso.xfer_size);
        }
    } else {
        transfer->actual_num_bytes = 0;
    }
    memset(pipe->qtds_nc_, 0, pipe->qtd_len_ * sizeof(usb_dwc_ll_dma_qtd_t));
    if (pipe->active_ == transfer) {
        pipe->active_ = nullptr;
        pipe->active_tail_ = nullptr;
    }
    transfer->status = status;
    post_locked(transfer, woken);
}

void Hcd::cancel_all_locked(Pipe* pipe, TransferStatus status, BaseType_t* woken) {
    Transfer* lists[] = {pipe->active_, pipe->pending_};
    pipe->active_ = pipe->active_tail_ = nullptr;
    pipe->pending_ = pipe->pending_tail_ = nullptr;
    for (Transfer* transfer : lists) {
        while (transfer) {
            Transfer* next = transfer->next;
            transfer->next = nullptr;
            transfer->status = status;
            transfer->actual_num_bytes = 0;
            for (int i = 0; i < transfer->num_isoc_packets; i++) {
                transfer->isoc_packet_desc[i].status = status;
                transfer->isoc_packet_desc[i].actual_num_bytes = 0;
            }
            post_locked(transfer, woken);
            transfer = next;
        }
    }
    if (pipe->qtds_nc_) memset(pipe->qtds_nc_, 0, pipe->qtd_len_ * sizeof(usb_dwc_ll_dma_qtd_t));
    pipe->used_slots_ = 0;
}

void Hcd::post_locked(Transfer* transfer, BaseType_t* woken) {
    (void)woken;
    transfer->done = true;
    transfer->next = nullptr;
    if (done_tail_) {
        done_tail_->next = transfer;
    } else {
        done_ = transfer;
    }
    done_tail_ = transfer;
    notify_pending_ = true;
}

void Hcd::post_port_locked(PortEvent event) {
    if (port_count_ == kPortEvents) return;
    port_events_[(port_head_ + port_count_) % kPortEvents] = event;
    port_count_++;
    notify_pending_ = true;
}

void Hcd::isr(void* arg) {
    auto* self = static_cast<Hcd*>(arg);
    BaseType_t woken = pdFALSE;
    portENTER_CRITICAL_ISR(&s_lock);
    const usb_dwc_hal_port_event_t event = usb_dwc_hal_decode_intr(&self->hal_);
    if (event == USB_DWC_HAL_PORT_EVENT_CHAN) {
        while (usb_dwc_hal_chan_t* chan = usb_dwc_hal_get_chan_pending_intr(&self->hal_)) {
            self->handle_channel(static_cast<Pipe*>(usb_dwc_hal_chan_get_context(chan)), &woken);
        }
    } else if (event != USB_DWC_HAL_PORT_EVENT_NONE) {
        self->handle_port(event);
    }
    const bool notify = self->notify_pending_;
    self->notify_pending_ = false;
    portEXIT_CRITICAL_ISR(&s_lock);
    if (notify) vTaskNotifyGiveFromISR(self->notify_, &woken);
    if (woken) portYIELD_FROM_ISR();
}

void Hcd::handle_port(usb_dwc_hal_port_event_t event) {
    switch (event) {
        case USB_DWC_HAL_PORT_EVENT_CONN:
            post_port_locked(PortEvent::Connected);
            break;
        case USB_DWC_HAL_PORT_EVENT_DISCONN:
            enabled_ = false;
            for (int i = 0; i < max_pipes_; i++) {
                if (!pipes_[i]) continue;
                cancel_all_locked(pipes_[i], TransferStatus::NoDevice, nullptr);
                pipes_[i]->state_ = Pipe::State::Dead;
            }
            post_port_locked(PortEvent::Disconnected);
            break;
        case USB_DWC_HAL_PORT_EVENT_ENABLED: {
            usb_dwc_hal_port_enable(&hal_);
            const usb_dwc_speed_t speed = usb_dwc_hal_port_get_conn_speed(&hal_);
            speed_ = speed == USB_DWC_SPEED_HIGH   ? Speed::High
                     : speed == USB_DWC_SPEED_LOW ? Speed::Low
                                                   : Speed::Full;
            enabled_ = true;
            break;
        }
        case USB_DWC_HAL_PORT_EVENT_DISABLED:
            enabled_ = false;
            if (!resetting_) post_port_locked(PortEvent::Error);
            break;
        case USB_DWC_HAL_PORT_EVENT_OVRCUR:
        case USB_DWC_HAL_PORT_EVENT_OVRCUR_CLR:
            usb_dwc_hal_port_toggle_power(&hal_, false);
            enabled_ = false;
            post_port_locked(PortEvent::Error);
            break;
        default:
            break;
    }
}

void Hcd::handle_channel(Pipe* pipe, BaseType_t* woken) {
    if (!pipe || !pipe->has_chan_) return;
    if (pipe->type_ == TransferType::Isochronous) {
        handle_isoc(pipe, woken);
        return;
    }
    const usb_dwc_hal_chan_event_t event = usb_dwc_hal_chan_decode_intr(&pipe->chan_);
    Transfer* transfer = pipe->active_;
    switch (event) {
        case USB_DWC_HAL_CHAN_EVENT_CPLT:
            if (!transfer) break;
            if (pipe->type_ == TransferType::Control && pipe->ctrl_stage_ < 2) {
                bool in;
                if (pipe->ctrl_stage_ == 0 && pipe->ctrl_skip_) {
                    in = true;
                    pipe->ctrl_stage_ = 2;
                } else if (pipe->ctrl_stage_ == 0) {
                    in = pipe->ctrl_in_;
                    pipe->ctrl_stage_ = 1;
                } else {
                    in = !pipe->ctrl_in_;
                    pipe->ctrl_stage_ = 2;
                }
                usb_dwc_hal_chan_set_dir(&pipe->chan_, in);
                usb_dwc_hal_chan_set_pid(&pipe->chan_, 1);
                usb_dwc_hal_chan_activate(&pipe->chan_, pipe->qtds_, kCtrlQtds, pipe->ctrl_stage_);
                break;
            }
            finish_locked(pipe, transfer, TransferStatus::Completed, woken);
            if (pipe->state_ == Pipe::State::Active) start_locked(pipe);
            break;
        case USB_DWC_HAL_CHAN_EVENT_ERROR:
            // The next SETUP clears a control endpoint's STALL or error.
            if (pipe->type_ != TransferType::Control && pipe->state_ != Pipe::State::Dead) {
                pipe->state_ = Pipe::State::Halted;
            }
            if (transfer) {
                finish_locked(pipe, transfer, chan_error_status(usb_dwc_hal_chan_get_error(&pipe->chan_)),
                              woken);
            }
            if (pipe->type_ == TransferType::Control && pipe->state_ == Pipe::State::Active) {
                start_locked(pipe);
            }
            break;
        case USB_DWC_HAL_CHAN_EVENT_HALT_REQ:
            if (pipe->state_ == Pipe::State::Halting) pipe->state_ = Pipe::State::Halted;
            xSemaphoreGiveFromISR(pipe->halted_, woken);
            break;
        default:
            break;
    }
}

void Hcd::handle_isoc(Pipe* pipe, BaseType_t* woken) {
    const uint32_t intrs = usb_dwc_ll_hcint_read_and_clear_intrs(pipe->chan_.regs);
    if (intrs & USB_DWC_LL_INTR_CHAN_CHHLTD) {
        const bool requested = pipe->chan_.flags.halt_requested;
        pipe->chan_.flags.halt_requested = 0;
        pipe->chan_.flags.active = 0;
        pipe->running_ = false;
        pipe->stopping_ = false;
        reap_isoc_locked(pipe, true, woken);
        pipe->used_slots_ = 0;
        if (requested) {
            if (pipe->state_ == Pipe::State::Halting) pipe->state_ = Pipe::State::Halted;
            xSemaphoreGiveFromISR(pipe->halted_, woken);
        } else if (pipe->state_ == Pipe::State::Active) {
            start_locked(pipe);
        }
        return;
    }
    if (!(intrs & USB_DWC_LL_INTR_CHAN_XFERCOMPL)) return;
    reap_isoc_locked(pipe, false, woken);
    if (pipe->state_ != Pipe::State::Active) return;
    while (pipe->pending_ && fill_isoc_locked(pipe, pipe->pending_)) {
    }
    if (!pipe->active_ && !pipe->stopping_) {
        pipe->stopping_ = true;
        usb_dwc_ll_hcchar_disable_chan(pipe->chan_.regs);
    }
}

}  // namespace usb_host::detail
