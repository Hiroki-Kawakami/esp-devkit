/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "dcd.hpp"

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
#include "soc/usb_dwc_struct.h"
#include "soc/usb_periph.h"

namespace usb_device::detail {

namespace {

const char* TAG = "usb_device";

constexpr int kHighSpeedController = 0;
constexpr uint32_t kForceModeDelayMs = 25;
constexpr uint32_t kTurnaroundUtmi16 = 5;
constexpr uint32_t kTimeoutCalibration = 5;
constexpr uint32_t kCoreRev420a = 0x4f54420a;
constexpr uint32_t kEp0TxFifoWords = kEp0MaxPacket / 4;
constexpr uint32_t kHighSpeedRxPacketBytes = 512;
constexpr uint32_t kFullSpeedRxPacketBytes = 64;

constexpr uint32_t kGotgctlAvalidOverride = 1u << 4;
constexpr uint32_t kGotgctlBvalidOverride = 1u << 6;
constexpr uint32_t kGotgctlBvalidValue = 1u << 7;

constexpr uint32_t kGahbcfgGlobalIntr = 1u << 0;
constexpr uint32_t kGahbcfgBurstIncr4 = 3u << 1;
constexpr uint32_t kGahbcfgDmaEnable = 1u << 5;

constexpr uint32_t kGusbcfgTimeoutCalMask = 0x7u;
constexpr uint32_t kGusbcfgPhyIf16 = 1u << 3;
constexpr uint32_t kGusbcfgUlpiUtmiSel = 1u << 4;
constexpr uint32_t kGusbcfgPhySel = 1u << 6;
constexpr uint32_t kGusbcfgSrpCap = 1u << 8;
constexpr uint32_t kGusbcfgHnpCap = 1u << 9;
constexpr uint32_t kGusbcfgTurnaroundShift = 10;
constexpr uint32_t kGusbcfgTurnaroundMask = 0xfu << kGusbcfgTurnaroundShift;
constexpr uint32_t kGusbcfgForceHost = 1u << 29;
constexpr uint32_t kGusbcfgForceDevice = 1u << 30;

constexpr uint32_t kGrstctlCoreReset = 1u << 0;
constexpr uint32_t kGrstctlRxFlush = 1u << 4;
constexpr uint32_t kGrstctlTxFlush = 1u << 5;
constexpr uint32_t kGrstctlTxFifoAll = 0x10u << 6;
constexpr uint32_t kGrstctlCoreResetDone = 1u << 29;
constexpr uint32_t kGrstctlAhbIdle = 1u << 31;

constexpr uint32_t kGintUsbSuspend = 1u << 11;
constexpr uint32_t kGintUsbReset = 1u << 12;
constexpr uint32_t kGintEnumDone = 1u << 13;
constexpr uint32_t kGintInEndpoint = 1u << 18;
constexpr uint32_t kGintOutEndpoint = 1u << 19;
constexpr uint32_t kGintWakeUp = 1u << 31;

constexpr uint32_t kDcfgSpeedHigh = 0;
constexpr uint32_t kDcfgSpeedFull = 1;
constexpr uint32_t kDcfgNonZeroStatusOutStall = 1u << 2;
constexpr uint32_t kDcfgAddressShift = 4;
constexpr uint32_t kDcfgAddressMask = 0x7fu << kDcfgAddressShift;
constexpr uint32_t kDcfgDescriptorDma = 1u << 23;

constexpr uint32_t kDctlSoftDisconnect = 1u << 1;

constexpr uint32_t kDstsSpeedShift = 1;
constexpr uint32_t kDstsSpeedMask = 0x3u << kDstsSpeedShift;
constexpr uint32_t kDstsSpeedHigh = 0;

constexpr uint32_t kEpctlEp0MpsMask = 0x3u;
constexpr uint32_t kEpctlStall = 1u << 21;
constexpr uint32_t kEpctlClearNak = 1u << 26;
constexpr uint32_t kEpctlSetNak = 1u << 27;
constexpr uint32_t kEpctlDisable = 1u << 30;
constexpr uint32_t kEpctlEnable = 1u << 31;

constexpr uint32_t kEpintXferComplete = 1u << 0;
constexpr uint32_t kEpintAhbError = 1u << 2;
constexpr uint32_t kEpintSetup = 1u << 3;
constexpr uint32_t kEpintTimeout = 1u << 3;
constexpr uint32_t kEpintStatusPhase = 1u << 5;
constexpr uint32_t kEpintBufferNotAvailable = 1u << 9;
constexpr uint32_t kEpintSetupReceived = 1u << 15;

constexpr uint32_t kDaintInEp0 = 1u << 0;
constexpr uint32_t kDaintOutEp0 = 1u << 16;

constexpr uint32_t kPcgcctlClocksStopped = 0xfu;

constexpr uint32_t kDescHostReady = 0u << 30;
constexpr uint32_t kDescLast = 1u << 27;
constexpr uint32_t kDescShort = 1u << 26;
constexpr uint32_t kDescIoc = 1u << 25;
constexpr uint32_t kDescSetupReceived = 1u << 24;
constexpr uint32_t kDescBytesMask = 0xffffu;

struct DmaDesc {
    uint32_t status;
    uint32_t buffer;
};

enum class Ep0Stage : uint8_t { Idle, Setup, Reply, DataIn, StatusOut, StatusIn };

size_t cache_line() {
    size_t alignment = 0;
    esp_cache_get_alignment(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL, &alignment);
    return alignment ? alignment : 64;
}

size_t round_up(size_t value, size_t alignment) {
    return (value + alignment - 1) / alignment * alignment;
}

template <typename T>
T* uncached(T* cached) {
    return reinterpret_cast<T*>(CACHE_LL_L2MEM_NON_CACHE_ADDR(cached));
}

uint32_t dma_address(const void* cached) {
    return static_cast<uint32_t>(reinterpret_cast<uintptr_t>(cached));
}

void memory_barrier() {
    __asm__ __volatile__("fence" ::: "memory");
}

void wait_bits_clear(volatile uint32_t* reg, uint32_t bits) {
    while (*reg & bits) {
    }
}

}  // namespace

struct Dcd::State {
    usb_dwc_dev_t* dev = nullptr;
    Port port = Port::HighSpeed;
    usb_phy_handle_t phy = nullptr;
    intr_handle_t intr = nullptr;
    QueueHandle_t events = nullptr;
    portMUX_TYPE lock = portMUX_INITIALIZER_UNLOCKED;

    uint8_t* memory = nullptr;
    DmaDesc* out_desc = nullptr;
    DmaDesc* in_desc = nullptr;
    uint8_t* setup_buffer = nullptr;
    uint8_t* ep0_buffer = nullptr;
    size_t ep0_bytes = 0;

    Ep0Stage stage = Ep0Stage::Idle;
    uint32_t fifo_limit = 0;
    uint32_t endpoints = 0;

    static constexpr int kMaxPending = 4;
    Event pending[kMaxPending] = {};
    int pending_count = 0;

    static void isr(void* arg);
    void handle_interrupt();
    void post(EventType type, Speed speed = Speed::Full, const SetupPacket* setup = nullptr);

    esp_err_t allocate(size_t bytes);
    void release();
    esp_err_t core_init();
    void core_reset();
    void flush_fifos();
    void bus_reset();
    void enumerated();

    void arm_out(Ep0Stage next, bool clear_nak);
    void arm_in(Ep0Stage next, size_t bytes, bool zlp);
    void ep0_out_interrupt();
    void ep0_in_interrupt();
};

esp_err_t Dcd::State::allocate(size_t bytes) {
    const size_t line = cache_line();
    ep0_bytes = round_up(bytes, line);
    const size_t total = 3 * line + ep0_bytes;
    memory = static_cast<uint8_t*>(
        heap_caps_aligned_calloc(line, 1, total, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
    if (!memory) return ESP_ERR_NO_MEM;
    esp_cache_msync(memory, total, ESP_CACHE_MSYNC_FLAG_DIR_C2M);

    out_desc = reinterpret_cast<DmaDesc*>(memory);
    in_desc = reinterpret_cast<DmaDesc*>(memory + line);
    setup_buffer = memory + 2 * line;
    ep0_buffer = memory + 3 * line;
    return ESP_OK;
}

void Dcd::State::release() {
    if (intr) esp_intr_free(intr);
    if (phy) usb_del_phy(phy);
    heap_caps_free(memory);
    intr = nullptr;
    phy = nullptr;
    memory = nullptr;
}

void Dcd::State::core_reset() {
    while (!(dev->grstctl_reg.val & kGrstctlAhbIdle)) {
    }
    const uint32_t snpsid = dev->gsnpsid_reg.val;
    dev->grstctl_reg.val |= kGrstctlCoreReset;
    if (snpsid < kCoreRev420a) {
        wait_bits_clear(&dev->grstctl_reg.val, kGrstctlCoreReset);
    } else {
        while (!(dev->grstctl_reg.val & kGrstctlCoreResetDone)) {
        }
        dev->grstctl_reg.val =
            (dev->grstctl_reg.val & ~kGrstctlCoreReset) | kGrstctlCoreResetDone;
    }
    while (!(dev->grstctl_reg.val & kGrstctlAhbIdle)) {
    }
}

void Dcd::State::flush_fifos() {
    dev->grstctl_reg.val = kGrstctlTxFlush | kGrstctlTxFifoAll;
    wait_bits_clear(&dev->grstctl_reg.val, kGrstctlTxFlush);
    dev->grstctl_reg.val = kGrstctlRxFlush;
    wait_bits_clear(&dev->grstctl_reg.val, kGrstctlRxFlush);
}

esp_err_t Dcd::State::core_init() {
    dev->gahbcfg_reg.val &= ~kGahbcfgGlobalIntr;

    uint32_t usbcfg = dev->gusbcfg_reg.val;
    usbcfg &= ~(kGusbcfgPhySel | kGusbcfgUlpiUtmiSel);
    usbcfg |= kGusbcfgPhyIf16;
    dev->gusbcfg_reg.val = usbcfg;
    core_reset();

    usbcfg = dev->gusbcfg_reg.val;
    usbcfg &= ~(kGusbcfgTurnaroundMask | kGusbcfgTimeoutCalMask | kGusbcfgForceHost |
                kGusbcfgHnpCap | kGusbcfgSrpCap);
    usbcfg |= (kTurnaroundUtmi16 << kGusbcfgTurnaroundShift) | kTimeoutCalibration |
              kGusbcfgForceDevice;
    dev->gusbcfg_reg.val = usbcfg;
    vTaskDelay(pdMS_TO_TICKS(kForceModeDelayMs));

    // Boot leaves the PHY suspended with its clock stopped and B-valid
    // overridden low; without VBUS sensing B-valid has to be forced high.
    dev->pcgcctl_reg.val &= ~kPcgcctlClocksStopped;
    dev->gotgctl_reg.val = (dev->gotgctl_reg.val & ~kGotgctlAvalidOverride) |
                           kGotgctlBvalidOverride | kGotgctlBvalidValue;

    dev->dctl_reg.val |= kDctlSoftDisconnect;
    dev->dcfg_reg.val = (port == Port::HighSpeed ? kDcfgSpeedHigh : kDcfgSpeedFull) |
                        kDcfgNonZeroStatusOutStall | kDcfgDescriptorDma;

    endpoints = dev->ghwcfg2_reg.numdeveps + 1;
    fifo_limit = dev->gdfifocfg_reg.epinfobaseaddr;
    flush_fifos();

    dev->diepmsk_reg.val = kEpintXferComplete | kEpintTimeout | kEpintAhbError |
                           kEpintBufferNotAvailable;
    dev->doepmsk_reg.val = kEpintXferComplete | kEpintSetup | kEpintStatusPhase |
                           kEpintAhbError | kEpintBufferNotAvailable;
    dev->daintmsk_reg.val = 0;
    dev->gotgint_reg.val = UINT32_MAX;
    dev->gintsts_reg.val = UINT32_MAX;
    dev->gintmsk_reg.val = kGintUsbReset | kGintEnumDone | kGintUsbSuspend | kGintWakeUp |
                           kGintInEndpoint | kGintOutEndpoint;
    dev->gahbcfg_reg.val = kGahbcfgGlobalIntr | kGahbcfgDmaEnable | kGahbcfgBurstIncr4;
    return ESP_OK;
}

void Dcd::State::bus_reset() {
    stage = Ep0Stage::Idle;
    dev->doepctl0_reg.val |= kEpctlSetNak;
    if (dev->diepctl0_reg.val & kEpctlEnable) {
        dev->diepctl0_reg.val |= kEpctlSetNak | kEpctlDisable;
    }
    for (uint32_t n = 1; n < endpoints; n++) {
        dev->out_eps[n - 1].doepctl_reg.val |= kEpctlSetNak;
        if (dev->in_eps[n - 1].diepctl_reg.val & kEpctlEnable) {
            dev->in_eps[n - 1].diepctl_reg.val |= kEpctlSetNak | kEpctlDisable;
        }
    }
    dev->daintmsk_reg.val = kDaintInEp0 | kDaintOutEp0;
    flush_fifos();

    const uint32_t packet = port == Port::HighSpeed ? kHighSpeedRxPacketBytes
                                                    : kFullSpeedRxPacketBytes;
    const uint32_t rx_words = 14 + 2 * (packet / 4 + 1) + 2 * endpoints;
    if (rx_words + kEp0TxFifoWords > fifo_limit) {
        ESP_DRAM_LOGE(TAG, "fifo: %u words needed, %u available",
                      (unsigned)(rx_words + kEp0TxFifoWords), (unsigned)fifo_limit);
    }
    dev->grxfsiz_reg.val = rx_words;
    dev->gnptxfsiz_reg.val = (kEp0TxFifoWords << 16) | rx_words;
    dev->dcfg_reg.val &= ~kDcfgAddressMask;
}

void Dcd::State::enumerated() {
    const uint32_t speed_bits = (dev->dsts_reg.val & kDstsSpeedMask) >> kDstsSpeedShift;
    const Speed speed = speed_bits == kDstsSpeedHigh ? Speed::High : Speed::Full;
    dev->diepctl0_reg.val &= ~kEpctlEp0MpsMask;
    dev->doepctl0_reg.val &= ~kEpctlEp0MpsMask;
    arm_out(Ep0Stage::Setup, false);
    post(EventType::Enumerated, speed);
}

// SETUP and the status OUT share the descriptor and its buffer: a SETUP is
// accepted whatever the stage, and lands wherever OUT is armed.
void Dcd::State::arm_out(Ep0Stage next, bool clear_nak) {
    stage = next;
    DmaDesc* desc = uncached(out_desc);
    desc->buffer = dma_address(setup_buffer);
    desc->status = kDescHostReady | kDescLast | kDescIoc | kEp0MaxPacket;
    memory_barrier();
    dev->doepdma0_reg.val = dma_address(out_desc);
    dev->doepctl0_reg.val |= kEpctlEnable | (clear_nak ? kEpctlClearNak : 0);
}

void Dcd::State::arm_in(Ep0Stage next, size_t bytes, bool zlp) {
    stage = next;
    uint32_t status = kDescHostReady | kDescLast | kDescIoc | static_cast<uint32_t>(bytes);
    if (bytes % kEp0MaxPacket != 0 || zlp) status |= kDescShort;
    DmaDesc* desc = uncached(in_desc);
    desc->buffer = dma_address(ep0_buffer);
    desc->status = status;
    memory_barrier();
    dev->diepdma0_reg.val = dma_address(in_desc);
    dev->diepctl0_reg.val |= kEpctlEnable | kEpctlClearNak;
}

void Dcd::State::ep0_out_interrupt() {
    const uint32_t ints = dev->doepint0_reg.val;
    dev->doepint0_reg.val = ints;

    // The packet is read on the setup-phase-done interrupt, not on the transfer
    // completion that precedes it: the host may still be resending it.
    if (ints & kEpintSetup) {
        const uint32_t status = uncached(out_desc)->status;
        const uint32_t received = kEp0MaxPacket - (status & kDescBytesMask);
        const uint32_t offset = received >= sizeof(SetupPacket) ? received - sizeof(SetupPacket)
                                                                 : 0;
        SetupPacket setup;
        memcpy(&setup, uncached(setup_buffer) + offset, sizeof(setup));
        stage = Ep0Stage::Reply;
        post(EventType::Setup, Speed::Full, &setup);
        return;
    }
    if ((ints & kEpintXferComplete) && !(ints & kEpintSetupReceived) &&
        stage == Ep0Stage::StatusOut) {
        if (uncached(out_desc)->status & kDescSetupReceived) {
            stage = Ep0Stage::Setup;
        } else {
            arm_out(Ep0Stage::Setup, false);
        }
    }
    if (ints & (kEpintAhbError | kEpintBufferNotAvailable)) {
        ESP_DRAM_LOGE(TAG, "ep0 out: 0x%08x", (unsigned)ints);
    }
}

void Dcd::State::ep0_in_interrupt() {
    const uint32_t ints = dev->diepint0_reg.val;
    dev->diepint0_reg.val = ints;

    if (ints & kEpintXferComplete) {
        if (stage == Ep0Stage::DataIn) {
            arm_out(Ep0Stage::StatusOut, true);
        } else if (stage == Ep0Stage::StatusIn) {
            arm_out(Ep0Stage::Setup, false);
        }
    }
    if (ints & (kEpintAhbError | kEpintBufferNotAvailable)) {
        ESP_DRAM_LOGE(TAG, "ep0 in: 0x%08x", (unsigned)ints);
    }
}

void Dcd::State::post(EventType type, Speed speed, const SetupPacket* setup) {
    if (pending_count == kMaxPending) return;
    Event& event = pending[pending_count++];
    event.type = type;
    event.speed = speed;
    if (setup) event.setup = *setup;
}

void Dcd::State::handle_interrupt() {
    const uint32_t ints = dev->gintsts_reg.val & dev->gintmsk_reg.val;

    if (ints & kGintUsbReset) {
        dev->gintsts_reg.val = kGintUsbReset;
        bus_reset();
        post(EventType::Reset);
    }
    if (ints & kGintEnumDone) {
        dev->gintsts_reg.val = kGintEnumDone;
        enumerated();
    }
    if (ints & kGintUsbSuspend) {
        dev->gintsts_reg.val = kGintUsbSuspend;
        post(EventType::Suspend);
    }
    if (ints & kGintWakeUp) {
        dev->gintsts_reg.val = kGintWakeUp;
        post(EventType::Resume);
    }
    if (ints & (kGintOutEndpoint | kGintInEndpoint)) {
        const uint32_t daint = dev->daint_reg.val & dev->daintmsk_reg.val;
        if (daint & kDaintOutEp0) ep0_out_interrupt();
        if (daint & kDaintInEp0) ep0_in_interrupt();
    }
}

void Dcd::State::isr(void* arg) {
    auto* self = static_cast<State*>(arg);
    portENTER_CRITICAL_ISR(&self->lock);
    self->handle_interrupt();
    Event events[kMaxPending];
    const int count = self->pending_count;
    memcpy(events, self->pending, sizeof(Event) * count);
    self->pending_count = 0;
    portEXIT_CRITICAL_ISR(&self->lock);

    BaseType_t woken = pdFALSE;
    for (int i = 0; i < count; i++) xQueueSendFromISR(self->events, &events[i], &woken);
    if (woken) portYIELD_FROM_ISR();
}

Dcd::Dcd() = default;

Dcd::~Dcd() {
    stop();
}

esp_err_t Dcd::start(Port port, size_t ep0_bytes, QueueHandle_t events) {
    if (state_) return ESP_ERR_INVALID_STATE;
    if (port != Port::HighSpeed) return ESP_ERR_NOT_SUPPORTED;

    auto* s = new (std::nothrow) State();
    if (!s) return ESP_ERR_NO_MEM;
    s->port = port;
    s->events = events;
    s->dev = &USB_DWC_HS;

    esp_err_t err = s->allocate(ep0_bytes);
    if (err == ESP_OK) {
        usb_phy_config_t phy = {};
        phy.controller = USB_PHY_CTRL_OTG;
        phy.target = USB_PHY_TARGET_UTMI;
        phy.otg_mode = USB_OTG_MODE_DEVICE;
        phy.otg_speed = USB_PHY_SPEED_HIGH;
        err = usb_new_phy(&phy, &s->phy);
        if (err != ESP_OK) ESP_LOGE(TAG, "phy: %s", esp_err_to_name(err));
    }
    if (err == ESP_OK) err = s->core_init();
    if (err == ESP_OK) {
        err = esp_intr_alloc(usb_dwc_info.controllers[kHighSpeedController].irq,
                             ESP_INTR_FLAG_INTRDISABLED, State::isr, s, &s->intr);
        if (err != ESP_OK) ESP_LOGE(TAG, "interrupt: %s", esp_err_to_name(err));
    }
    if (err != ESP_OK) {
        s->release();
        delete s;
        return err;
    }

    state_ = s;
    esp_intr_enable(s->intr);
    s->dev->dctl_reg.val &= ~kDctlSoftDisconnect;
    return ESP_OK;
}

void Dcd::stop() {
    if (!state_) return;
    state_->dev->dctl_reg.val |= kDctlSoftDisconnect;
    state_->dev->gahbcfg_reg.val &= ~kGahbcfgGlobalIntr;
    state_->release();
    delete state_;
    state_ = nullptr;
}

uint8_t* Dcd::ep0_buffer() {
    return uncached(state_->ep0_buffer);
}

void Dcd::ep0_send(size_t bytes, bool zlp) {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply) state_->arm_in(Ep0Stage::DataIn, bytes, zlp);
    portEXIT_CRITICAL(&state_->lock);
}

void Dcd::ep0_ack() {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply) state_->arm_in(Ep0Stage::StatusIn, 0, false);
    portEXIT_CRITICAL(&state_->lock);
}

void Dcd::ep0_stall() {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply) {
        state_->dev->diepctl0_reg.val |= kEpctlStall;
        state_->dev->doepctl0_reg.val |= kEpctlStall;
        state_->arm_out(Ep0Stage::Setup, false);
    }
    portEXIT_CRITICAL(&state_->lock);
}

void Dcd::set_address(uint8_t address) {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply) {
        usb_dwc_dev_t* dev = state_->dev;
        dev->dcfg_reg.val = (dev->dcfg_reg.val & ~kDcfgAddressMask) |
                            (static_cast<uint32_t>(address) << kDcfgAddressShift);
    }
    portEXIT_CRITICAL(&state_->lock);
}

}  // namespace usb_device::detail
