/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "dcd.hpp"

#include <algorithm>
#include <cstring>
#include <new>

#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_intr_alloc.h"
#include "esp_log.h"
#include "esp_private/esp_cache_private.h"
#include "esp_private/usb_phy.h"
#include "esp_rom_sys.h"
#include "freertos/task.h"
#include "hal/cache_ll.h"
#include "soc/usb_dwc_struct.h"
#include "soc/usb_periph.h"

namespace usb_device::detail {

namespace {

const char* TAG = "usb_device";

constexpr int kHighSpeedController = 0;
constexpr int kMaxEndpoints = 16;
constexpr int kDescriptorsPerEndpoint = 16;
constexpr uint32_t kForceModeDelayMs = 25;
constexpr uint32_t kTurnaroundUtmi16 = 5;
constexpr uint32_t kTimeoutCalibration = 5;
constexpr uint32_t kCoreRev420a = 0x4f54420a;
constexpr uint32_t kEp0TxFifoWords = kEp0MaxPacket / 4;
constexpr uint32_t kHighSpeedRxPacketBytes = 512;
constexpr uint32_t kFullSpeedRxPacketBytes = 64;
constexpr uint32_t kTxFifoPackets = 2;
constexpr uint8_t kTxFifos = sizeof(usb_dwc_dev_t::dieptxfi_regs) / sizeof(usb_dwc_dieptxfi_reg_t);
constexpr uint32_t kDisableTimeoutUs = 1000;

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
constexpr uint32_t kGrstctlTxFifoShift = 6;
constexpr uint32_t kGrstctlTxFifoAll = 0x10u << kGrstctlTxFifoShift;
constexpr uint32_t kGrstctlCoreResetDone = 1u << 29;
constexpr uint32_t kGrstctlAhbIdle = 1u << 31;

constexpr uint32_t kGintOutNakEffective = 1u << 7;
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
constexpr uint32_t kDctlSetGlobalOutNak = 1u << 9;
constexpr uint32_t kDctlClearGlobalOutNak = 1u << 10;

constexpr uint32_t kDstsSpeedShift = 1;
constexpr uint32_t kDstsSpeedMask = 0x3u << kDstsSpeedShift;
constexpr uint32_t kDstsSpeedHigh = 0;

constexpr uint32_t kEpctlEp0MpsMask = 0x3u;
constexpr uint32_t kEpctlActive = 1u << 15;
constexpr uint32_t kEpctlTypeShift = 18;
constexpr uint32_t kEpctlStall = 1u << 21;
constexpr uint32_t kEpctlTxFifoShift = 22;
constexpr uint32_t kEpctlClearNak = 1u << 26;
constexpr uint32_t kEpctlSetNak = 1u << 27;
constexpr uint32_t kEpctlSetData0 = 1u << 28;
constexpr uint32_t kEpctlDisable = 1u << 30;
constexpr uint32_t kEpctlEnable = 1u << 31;

constexpr uint32_t kEpintXferComplete = 1u << 0;
constexpr uint32_t kEpintDisabled = 1u << 1;
constexpr uint32_t kEpintAhbError = 1u << 2;
constexpr uint32_t kEpintSetup = 1u << 3;
constexpr uint32_t kEpintTimeout = 1u << 3;
constexpr uint32_t kEpintStatusPhase = 1u << 5;
constexpr uint32_t kEpintInNakEffective = 1u << 6;
constexpr uint32_t kEpintBufferNotAvailable = 1u << 9;
constexpr uint32_t kEpintSetupReceived = 1u << 15;

constexpr uint32_t kDaintOutShift = 16;

constexpr uint32_t kPcgcctlClocksStopped = 0xfu;

constexpr uint32_t kDescStatusShift = 30;
constexpr uint32_t kDescHostReady = 0u << kDescStatusShift;
constexpr uint32_t kDescDmaDone = 2u;
constexpr uint32_t kDescErrorMask = 3u << 28;
constexpr uint32_t kDescLast = 1u << 27;
constexpr uint32_t kDescShort = 1u << 26;
constexpr uint32_t kDescIoc = 1u << 25;
constexpr uint32_t kDescSetupReceived = 1u << 24;
constexpr uint32_t kDescBytesMask = 0xffffu;

struct DmaDesc {
    uint32_t status;
    uint32_t buffer;
};

enum class Ep0Stage : uint8_t {
    Idle,
    Setup,
    Reply,
    DataIn,
    DataOut,
    ReplyOut,
    StatusOut,
    StatusIn,
};

struct Ep {
    bool open = false;
    bool in = false;
    bool stalled = false;
    uint8_t number = 0;
    uint16_t max_packet_bytes = 0;
    uint32_t chunk = 0;
    DmaDesc* descs = nullptr;
    int desc_count = 0;
    Transfer* active = nullptr;
    Transfer* head = nullptr;
    Transfer* tail = nullptr;
};

size_t cache_line(uint32_t caps) {
    size_t alignment = 0;
    esp_cache_get_alignment(caps, &alignment);
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

bool wait_bits_set(volatile uint32_t* reg, uint32_t bits) {
    for (uint32_t us = 0; us < kDisableTimeoutUs; us++) {
        if (*reg & bits) return true;
        esp_rom_delay_us(1);
    }
    return false;
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
    size_t out_alignment = 0;

    Ep0Stage stage = Ep0Stage::Idle;
    uint32_t ep0_expected = 0;
    uint32_t ep0_received = 0;
    bool status_phase = false;
    bool status_pending = false;

    uint32_t fifo_limit = 0;
    uint32_t endpoints = 0;
    uint32_t tx_fifo_base = 0;
    uint32_t tx_fifo_next = 0;
    uint8_t tx_fifo_count = 0;
    uint8_t tx_fifo[kMaxEndpoints] = {};

    Ep in_eps[kMaxEndpoints];
    Ep out_eps[kMaxEndpoints];

    Transfer* done_head = nullptr;
    Transfer* done_tail = nullptr;
    bool done_posted = false;

    static constexpr int kMaxPending = 4;
    Event pending[kMaxPending] = {};
    int pending_count = 0;

    static void isr(void* arg);
    void handle_interrupt();
    void post(EventType type, Speed speed = Speed::Full, const SetupPacket* setup = nullptr,
              uint16_t length = 0);
    int take_pending(Event* out);
    void send_pending_from_task();

    esp_err_t allocate(size_t bytes);
    void release();
    esp_err_t core_init();
    void core_reset();
    void flush_fifos();
    void flush_tx_fifo(uint32_t fifo);
    void bus_reset();
    void enumerated();

    void arm_out(Ep0Stage next, bool clear_nak);
    void arm_out_data();
    void arm_in(Ep0Stage next, size_t bytes, bool zlp);
    void ep0_out_interrupt();
    void ep0_in_interrupt();

    Ep* endpoint(uint8_t address);
    volatile uint32_t& ctl(const Ep& ep);
    volatile uint32_t& ints(const Ep& ep);
    volatile uint32_t& dma(const Ep& ep);
    void start(Ep& ep);
    uint32_t progress(const Ep& ep) const;
    void finish(Ep& ep, TransferStatus status, uint32_t actual);
    void complete(Transfer* transfer, TransferStatus status, uint32_t actual);
    void cancel_all(Ep& ep, TransferStatus status);
    bool disable(Ep& ep);
    void endpoint_interrupt(Ep& ep);
};

esp_err_t Dcd::State::allocate(size_t bytes) {
    const size_t line = cache_line(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
    out_alignment = std::max(line, cache_line(MALLOC_CAP_DMA | MALLOC_CAP_SPIRAM));
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
    for (int n = 0; n < kMaxEndpoints; n++) {
        heap_caps_free(in_eps[n].descs);
        heap_caps_free(out_eps[n].descs);
        in_eps[n].descs = nullptr;
        out_eps[n].descs = nullptr;
    }
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

void Dcd::State::flush_tx_fifo(uint32_t fifo) {
    dev->grstctl_reg.val = kGrstctlTxFlush | (fifo << kGrstctlTxFifoShift);
    wait_bits_clear(&dev->grstctl_reg.val, kGrstctlTxFlush);
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

    endpoints = std::min<uint32_t>(dev->ghwcfg2_reg.numdeveps + 1, kMaxEndpoints);
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
    status_pending = false;
    dev->doepctl0_reg.val |= kEpctlSetNak;
    if (dev->diepctl0_reg.val & kEpctlEnable) {
        dev->diepctl0_reg.val |= kEpctlSetNak | kEpctlDisable;
    }
    for (uint32_t n = 1; n < endpoints; n++) {
        Ep* eps[2] = {&in_eps[n], &out_eps[n]};
        for (Ep* ep : eps) {
            ctl(*ep) |= kEpctlSetNak;
            if (ep->in && (ctl(*ep) & kEpctlEnable)) ctl(*ep) |= kEpctlDisable;
            if (!ep->open) continue;
            cancel_all(*ep, TransferStatus::Canceled);
            ep->open = false;
            ep->stalled = false;
        }
    }
    dev->daintmsk_reg.val = (1u << 0) | (1u << kDaintOutShift);
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
    tx_fifo_base = rx_words + kEp0TxFifoWords;
    tx_fifo_next = tx_fifo_base;
    tx_fifo_count = 0;
    memset(tx_fifo, 0, sizeof(tx_fifo));
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

void Dcd::State::arm_out_data() {
    stage = Ep0Stage::DataOut;
    DmaDesc* desc = uncached(out_desc);
    desc->buffer = dma_address(ep0_buffer + ep0_received);
    desc->status = kDescHostReady | kDescLast | kDescIoc | kEp0MaxPacket;
    memory_barrier();
    dev->doepdma0_reg.val = dma_address(out_desc);
    dev->doepctl0_reg.val |= kEpctlEnable | kEpctlClearNak;
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
    const DmaDesc* desc = uncached(out_desc);

    // The packet is read on the setup-phase-done interrupt, not on the transfer
    // completion that precedes it: the host may still be resending it.
    if (ints & kEpintSetup) {
        const uint32_t received = kEp0MaxPacket - (desc->status & kDescBytesMask);
        const uint32_t offset = received >= sizeof(SetupPacket) ? received - sizeof(SetupPacket)
                                                                 : 0;
        const auto* buffer = reinterpret_cast<const uint8_t*>(desc->buffer);
        SetupPacket setup;
        memcpy(&setup, uncached(buffer) + offset, sizeof(setup));
        stage = Ep0Stage::Reply;
        status_pending = false;
        post(EventType::Setup, Speed::Full, &setup);
        return;
    }
    if ((ints & kEpintXferComplete) && !(ints & kEpintSetupReceived) &&
        !(desc->status & kDescSetupReceived)) {
        if (stage == Ep0Stage::StatusOut) {
            arm_out(Ep0Stage::Setup, false);
        } else if (stage == Ep0Stage::DataOut) {
            const uint32_t packet = kEp0MaxPacket - (desc->status & kDescBytesMask);
            ep0_received += packet;
            if (ep0_received < ep0_expected && packet == kEp0MaxPacket) {
                arm_out_data();
            } else {
                stage = Ep0Stage::ReplyOut;
                post(EventType::Ep0Received, Speed::Full, nullptr,
                     static_cast<uint16_t>(ep0_received));
            }
        }
    } else if ((ints & kEpintXferComplete) && stage == Ep0Stage::StatusOut) {
        stage = Ep0Stage::Setup;
    }
    // In descriptor DMA the status IN of a control write has to wait for the
    // host to start the status stage.
    if (ints & kEpintStatusPhase) {
        status_phase = true;
        if (status_pending) {
            status_pending = false;
            arm_in(Ep0Stage::StatusIn, 0, false);
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

Ep* Dcd::State::endpoint(uint8_t address) {
    const uint8_t number = address & 0x7f;
    if (number == 0 || number >= endpoints) return nullptr;
    return (address & 0x80) ? &in_eps[number] : &out_eps[number];
}

volatile uint32_t& Dcd::State::ctl(const Ep& ep) {
    return ep.in ? dev->in_eps[ep.number - 1].diepctl_reg.val
                 : dev->out_eps[ep.number - 1].doepctl_reg.val;
}

volatile uint32_t& Dcd::State::ints(const Ep& ep) {
    return ep.in ? dev->in_eps[ep.number - 1].diepint_reg.val
                 : dev->out_eps[ep.number - 1].doepint_reg.val;
}

volatile uint32_t& Dcd::State::dma(const Ep& ep) {
    return ep.in ? dev->in_eps[ep.number - 1].diepdma_reg.val
                 : dev->out_eps[ep.number - 1].doepdma_reg.val;
}

// One transfer runs at a time, spread over as many descriptors as it needs.
// Every OUT descriptor interrupts, since a short packet may end the transfer
// in any of them; the controller then disables the endpoint itself and leaves
// the descriptors after it untouched.
void Dcd::State::start(Ep& ep) {
    Transfer* transfer = ep.head;
    ep.head = transfer->next;
    if (!ep.head) ep.tail = nullptr;
    transfer->next = nullptr;
    ep.active = transfer;

    DmaDesc* descs = uncached(ep.descs);
    uint32_t remaining = transfer->length;
    uint32_t offset = 0;
    int count = 0;
    do {
        const uint32_t bytes = std::min(remaining, ep.chunk);
        remaining -= bytes;
        uint32_t status = kDescHostReady | bytes;
        if (!ep.in) status |= kDescIoc;
        if (remaining == 0) {
            status |= kDescLast | kDescIoc;
            if (ep.in && (bytes % ep.max_packet_bytes != 0 || (transfer->zlp && bytes > 0))) {
                status |= kDescShort;
            }
        }
        descs[count].buffer = dma_address(transfer->buffer + offset);
        descs[count].status = status;
        offset += bytes;
        count++;
    } while (remaining > 0);
    ep.desc_count = count;

    memory_barrier();
    dma(ep) = dma_address(ep.descs);
    ctl(ep) |= kEpctlEnable | kEpctlClearNak;
}

uint32_t Dcd::State::progress(const Ep& ep) const {
    const DmaDesc* descs = uncached(ep.descs);
    uint32_t moved = 0;
    uint32_t remaining = ep.active->length;
    for (int i = 0; i < ep.desc_count; i++) {
        const uint32_t requested = std::min(remaining, ep.chunk);
        remaining -= requested;
        const uint32_t status = descs[i].status;
        if ((status >> kDescStatusShift) != kDescDmaDone) break;
        moved += requested - (status & kDescBytesMask);
    }
    return moved;
}

void Dcd::State::complete(Transfer* transfer, TransferStatus status, uint32_t actual) {
    transfer->status = status;
    transfer->actual = actual;
    transfer->next = nullptr;
    if (done_tail) {
        done_tail->next = transfer;
    } else {
        done_head = transfer;
    }
    done_tail = transfer;
    if (!done_posted) {
        done_posted = true;
        post(EventType::TransfersDone);
    }
}

void Dcd::State::finish(Ep& ep, TransferStatus status, uint32_t actual) {
    Transfer* transfer = ep.active;
    ep.active = nullptr;
    complete(transfer, status, actual);
    if (ep.head && !ep.stalled) start(ep);
}

void Dcd::State::cancel_all(Ep& ep, TransferStatus status) {
    if (ep.active) {
        const uint32_t moved = progress(ep);
        Transfer* transfer = ep.active;
        ep.active = nullptr;
        complete(transfer, status, moved);
    }
    while (Transfer* transfer = ep.head) {
        ep.head = transfer->next;
        complete(transfer, status, 0);
    }
    ep.tail = nullptr;
}

bool Dcd::State::disable(Ep& ep) {
    volatile uint32_t& control = ctl(ep);
    if (!(control & kEpctlEnable)) return true;
    bool ok;
    if (ep.in) {
        control |= kEpctlSetNak;
        ok = wait_bits_set(&ints(ep), kEpintInNakEffective);
        control |= kEpctlDisable | kEpctlSetNak;
        ok = wait_bits_set(&ints(ep), kEpintDisabled) && ok;
        ints(ep) = kEpintDisabled | kEpintInNakEffective;
        flush_tx_fifo(tx_fifo[ep.number]);
    } else {
        dev->dctl_reg.val |= kDctlSetGlobalOutNak;
        ok = wait_bits_set(&dev->gintsts_reg.val, kGintOutNakEffective);
        control |= kEpctlDisable | kEpctlSetNak;
        ok = wait_bits_set(&ints(ep), kEpintDisabled) && ok;
        ints(ep) = kEpintDisabled;
        dev->dctl_reg.val |= kDctlClearGlobalOutNak;
    }
    if (!ok) ESP_DRAM_LOGE(TAG, "disable ep 0x%02x timed out", ep.number | (ep.in ? 0x80 : 0));
    return ok;
}

void Dcd::State::endpoint_interrupt(Ep& ep) {
    const uint32_t flags = ints(ep);
    ints(ep) = flags;
    if ((flags & kEpintXferComplete) && ep.active) {
        if (ep.in) {
            finish(ep, TransferStatus::Completed, progress(ep));
        } else {
            const DmaDesc* descs = uncached(ep.descs);
            uint32_t moved = 0;
            uint32_t remaining = ep.active->length;
            bool ended = false;
            bool error = false;
            for (int i = 0; i < ep.desc_count && !ended; i++) {
                const uint32_t requested = std::min(remaining, ep.chunk);
                remaining -= requested;
                const uint32_t status = descs[i].status;
                if ((status >> kDescStatusShift) != kDescDmaDone) break;
                const uint32_t received = requested - (status & kDescBytesMask);
                moved += received;
                error = error || (status & kDescErrorMask);
                ended = received < requested || i == ep.desc_count - 1;
            }
            if (ended) {
                finish(ep, error ? TransferStatus::Error : TransferStatus::Completed, moved);
            }
        }
    }
    if (flags & (kEpintAhbError | kEpintBufferNotAvailable)) {
        ESP_DRAM_LOGE(TAG, "ep 0x%02x: 0x%08x", ep.number | (ep.in ? 0x80 : 0),
                      (unsigned)flags);
    }
}

void Dcd::State::post(EventType type, Speed speed, const SetupPacket* setup, uint16_t length) {
    if (pending_count == kMaxPending) {
        ESP_DRAM_LOGE(TAG, "event dropped");
        return;
    }
    Event& event = pending[pending_count++];
    event.type = type;
    event.speed = speed;
    event.length = length;
    if (setup) event.setup = *setup;
}

int Dcd::State::take_pending(Event* out) {
    const int count = pending_count;
    memcpy(out, pending, sizeof(Event) * count);
    pending_count = 0;
    return count;
}

void Dcd::State::send_pending_from_task() {
    Event out[kMaxPending];
    portENTER_CRITICAL(&lock);
    const int count = take_pending(out);
    portEXIT_CRITICAL(&lock);
    for (int i = 0; i < count; i++) xQueueSend(events, &out[i], 0);
}

void Dcd::State::handle_interrupt() {
    const uint32_t flags = dev->gintsts_reg.val & dev->gintmsk_reg.val;

    if (flags & kGintUsbReset) {
        dev->gintsts_reg.val = kGintUsbReset;
        bus_reset();
        post(EventType::Reset);
    }
    if (flags & kGintEnumDone) {
        dev->gintsts_reg.val = kGintEnumDone;
        enumerated();
    }
    if (flags & kGintUsbSuspend) {
        dev->gintsts_reg.val = kGintUsbSuspend;
        post(EventType::Suspend);
    }
    if (flags & kGintWakeUp) {
        dev->gintsts_reg.val = kGintWakeUp;
        post(EventType::Resume);
    }
    if (flags & (kGintOutEndpoint | kGintInEndpoint)) {
        const uint32_t daint = dev->daint_reg.val & dev->daintmsk_reg.val;
        if (daint & (1u << kDaintOutShift)) ep0_out_interrupt();
        if (daint & 1u) ep0_in_interrupt();
        for (uint32_t n = 1; n < endpoints; n++) {
            if (daint & (1u << (n + kDaintOutShift))) endpoint_interrupt(out_eps[n]);
            if (daint & (1u << n)) endpoint_interrupt(in_eps[n]);
        }
    }
}

void Dcd::State::isr(void* arg) {
    auto* self = static_cast<State*>(arg);
    Event events[kMaxPending];
    portENTER_CRITICAL_ISR(&self->lock);
    self->handle_interrupt();
    const int count = self->take_pending(events);
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
    for (int n = 0; n < kMaxEndpoints; n++) {
        s->in_eps[n].in = true;
        s->in_eps[n].number = static_cast<uint8_t>(n);
        s->out_eps[n].number = static_cast<uint8_t>(n);
    }

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

void Dcd::ep0_receive(size_t bytes) {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply) {
        state_->ep0_expected = static_cast<uint32_t>(bytes);
        state_->ep0_received = 0;
        state_->status_phase = false;
        state_->arm_out_data();
    }
    portEXIT_CRITICAL(&state_->lock);
}

void Dcd::ep0_ack() {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply) {
        state_->arm_in(Ep0Stage::StatusIn, 0, false);
    } else if (state_->stage == Ep0Stage::ReplyOut) {
        if (state_->status_phase) {
            state_->arm_in(Ep0Stage::StatusIn, 0, false);
        } else {
            state_->status_pending = true;
        }
    }
    portEXIT_CRITICAL(&state_->lock);
}

void Dcd::ep0_stall() {
    portENTER_CRITICAL(&state_->lock);
    if (state_->stage == Ep0Stage::Reply || state_->stage == Ep0Stage::ReplyOut) {
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

esp_err_t Dcd::open_endpoint(uint8_t address, EndpointType type, uint16_t max_packet_bytes) {
    if (!state_) return ESP_ERR_INVALID_STATE;
    State& s = *state_;
    Ep* ep = s.endpoint(address);
    if (!ep || max_packet_bytes == 0) return ESP_ERR_INVALID_ARG;
    if (type != EndpointType::Bulk && type != EndpointType::Interrupt) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (!ep->descs) {
        const size_t line = cache_line(MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
        const size_t bytes = round_up(sizeof(DmaDesc) * kDescriptorsPerEndpoint, line);
        ep->descs = static_cast<DmaDesc*>(
            heap_caps_aligned_calloc(line, 1, bytes, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL));
        if (!ep->descs) return ESP_ERR_NO_MEM;
        esp_cache_msync(ep->descs, bytes, ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    }

    esp_err_t err = ESP_OK;
    portENTER_CRITICAL(&s.lock);
    uint32_t fifo = 0;
    if (ep->open) {
        err = ESP_ERR_INVALID_STATE;
    } else if (ep->in) {
        fifo = s.tx_fifo[ep->number];
        if (fifo == 0) {
            const uint32_t words = kTxFifoPackets * ((max_packet_bytes + 3) / 4);
            if (s.tx_fifo_count >= kTxFifos || s.tx_fifo_next + words > s.fifo_limit) {
                err = ESP_ERR_NO_MEM;
            } else {
                fifo = ++s.tx_fifo_count;
                s.dev->dieptxfi_regs[fifo - 1].val = (words << 16) | s.tx_fifo_next;
                s.tx_fifo_next += words;
                s.tx_fifo[ep->number] = static_cast<uint8_t>(fifo);
                s.flush_tx_fifo(fifo);
            }
        }
    }
    if (err == ESP_OK) {
        ep->open = true;
        ep->stalled = false;
        ep->max_packet_bytes = max_packet_bytes;
        ep->chunk = kDescBytesMask / max_packet_bytes * max_packet_bytes;
        s.ctl(*ep) = max_packet_bytes | (static_cast<uint32_t>(type) << kEpctlTypeShift) |
                     kEpctlActive | kEpctlSetData0 | kEpctlSetNak |
                     (fifo << kEpctlTxFifoShift);
        s.dev->daintmsk_reg.val |= 1u << (ep->number + (ep->in ? 0 : kDaintOutShift));
    }
    portEXIT_CRITICAL(&s.lock);
    return err;
}

void Dcd::close_endpoint(uint8_t address) {
    if (!state_) return;
    State& s = *state_;
    Ep* ep = s.endpoint(address);
    if (!ep) return;
    portENTER_CRITICAL(&s.lock);
    if (ep->open) {
        s.disable(*ep);
        s.cancel_all(*ep, TransferStatus::Canceled);
        s.ctl(*ep) &= ~(kEpctlActive | kEpctlStall);
        s.dev->daintmsk_reg.val &= ~(1u << (ep->number + (ep->in ? 0 : kDaintOutShift)));
        ep->open = false;
        ep->stalled = false;
    }
    portEXIT_CRITICAL(&s.lock);
    s.send_pending_from_task();
}

esp_err_t Dcd::submit(Transfer* transfer) {
    if (!state_) return ESP_ERR_INVALID_STATE;
    State& s = *state_;
    Ep* ep = s.endpoint(transfer->endpoint);
    if (!ep) return ESP_ERR_INVALID_ARG;

    const auto address = reinterpret_cast<uintptr_t>(transfer->buffer);
    if (transfer->length > 0) {
        if (ep->in) {
            if (address % 4 != 0) return ESP_ERR_INVALID_ARG;
            esp_cache_msync(transfer->buffer, transfer->length,
                            ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED);
        } else {
            if (address % s.out_alignment != 0 || transfer->length % s.out_alignment != 0) {
                return ESP_ERR_INVALID_ARG;
            }
            esp_cache_msync(transfer->buffer, transfer->length, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
        }
    }

    esp_err_t err = ESP_OK;
    portENTER_CRITICAL(&s.lock);
    if (!ep->open) {
        err = ESP_ERR_INVALID_STATE;
    } else if ((!ep->in && (transfer->length == 0 ||
                            transfer->length % ep->max_packet_bytes != 0)) ||
               transfer->length > ep->chunk * kDescriptorsPerEndpoint) {
        err = ESP_ERR_INVALID_SIZE;
    } else {
        transfer->actual = 0;
        transfer->status = TransferStatus::Completed;
        transfer->next = nullptr;
        if (ep->tail) {
            ep->tail->next = transfer;
        } else {
            ep->head = transfer;
        }
        ep->tail = transfer;
        if (!ep->active && !ep->stalled) s.start(*ep);
    }
    portEXIT_CRITICAL(&s.lock);
    return err;
}

void Dcd::flush(uint8_t address) {
    if (!state_) return;
    State& s = *state_;
    Ep* ep = s.endpoint(address);
    if (!ep) return;
    portENTER_CRITICAL(&s.lock);
    if (ep->open) {
        s.disable(*ep);
        s.cancel_all(*ep, TransferStatus::Canceled);
    }
    portEXIT_CRITICAL(&s.lock);
    s.send_pending_from_task();
}

esp_err_t Dcd::set_stall(uint8_t address, bool stall) {
    if (!state_) return ESP_ERR_INVALID_STATE;
    State& s = *state_;
    Ep* ep = s.endpoint(address);
    if (!ep) return ESP_ERR_INVALID_ARG;

    esp_err_t err = ESP_OK;
    portENTER_CRITICAL(&s.lock);
    if (!ep->open) {
        err = ESP_ERR_INVALID_STATE;
    } else if (stall) {
        if (ep->in && ep->active) {
            s.disable(*ep);
            const uint32_t moved = s.progress(*ep);
            Transfer* transfer = ep->active;
            ep->active = nullptr;
            s.complete(transfer, TransferStatus::Stalled, moved);
        }
        s.ctl(*ep) |= kEpctlStall;
        ep->stalled = true;
    } else {
        s.ctl(*ep) = (s.ctl(*ep) & ~kEpctlStall) | kEpctlSetData0;
        ep->stalled = false;
        if (!ep->active && ep->head) s.start(*ep);
    }
    portEXIT_CRITICAL(&s.lock);
    s.send_pending_from_task();
    return err;
}

bool Dcd::stalled(uint8_t address) {
    if (!state_) return false;
    Ep* ep = state_->endpoint(address);
    return ep && ep->stalled;
}

Transfer* Dcd::take_done() {
    if (!state_) return nullptr;
    State& s = *state_;
    portENTER_CRITICAL(&s.lock);
    Transfer* transfer = s.done_head;
    if (transfer) {
        s.done_head = transfer->next;
        if (!s.done_head) s.done_tail = nullptr;
        transfer->next = nullptr;
    } else {
        s.done_posted = false;
    }
    portEXIT_CRITICAL(&s.lock);
    if (transfer && !(transfer->endpoint & 0x80) && transfer->actual > 0) {
        esp_cache_msync(transfer->buffer, transfer->length, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    }
    return transfer;
}

}  // namespace usb_device::detail
