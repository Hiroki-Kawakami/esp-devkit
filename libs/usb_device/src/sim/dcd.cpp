/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * USB/IP server standing in for the controller.
 */

#include "dcd.hpp"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <unistd.h>

#include <algorithm>
#include <condition_variable>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "esp_log.h"

namespace usb_device::detail {

namespace {

const char* TAG = "usb_device";

constexpr const char* kListenEnv = "SIMULATOR_USBD_LISTEN";
constexpr const char* kDefaultListen = "127.0.0.1:3240";

constexpr uint16_t kUsbipVersion = 0x0111;
constexpr uint16_t kOpReqDevlist = 0x8005;
constexpr uint16_t kOpRepDevlist = 0x0005;
constexpr uint16_t kOpReqImport = 0x8003;
constexpr uint16_t kOpRepImport = 0x0003;
constexpr uint32_t kCmdSubmit = 1;
constexpr uint32_t kCmdUnlink = 2;
constexpr uint32_t kRetSubmit = 3;
constexpr uint32_t kRetUnlink = 4;
constexpr uint32_t kDirIn = 1;
constexpr uint32_t kUrbZeroPacket = 0x0040;
constexpr uint32_t kNoIsoPackets = 0xffffffff;
constexpr size_t kHeaderBytes = 48;
constexpr size_t kBusIdBytes = 32;
constexpr size_t kPathBytes = 256;

constexpr int32_t kErrPipe = -32;
constexpr int32_t kErrConnReset = -104;
constexpr int32_t kErrShutdown = -108;

constexpr uint32_t kSpeedFull = 2;
constexpr uint32_t kSpeedHigh = 3;
constexpr uint32_t kBusNum = 1;
constexpr uint32_t kDevNum = 2;
constexpr const char* kBusId = "1-1";
constexpr const char* kPath = "/sys/devices/simulator/usb1/1-1";
constexpr int kMaxEndpoints = 16;

void put8(std::vector<uint8_t>& out, uint8_t value) {
    out.push_back(value);
}

void put16(std::vector<uint8_t>& out, uint16_t value) {
    out.push_back(static_cast<uint8_t>(value >> 8));
    out.push_back(static_cast<uint8_t>(value));
}

void put32(std::vector<uint8_t>& out, uint32_t value) {
    put16(out, static_cast<uint16_t>(value >> 16));
    put16(out, static_cast<uint16_t>(value));
}

void put_string(std::vector<uint8_t>& out, const char* text, size_t bytes) {
    const size_t length = std::min(strlen(text), bytes);
    out.insert(out.end(), text, text + length);
    out.insert(out.end(), bytes - length, 0);
}

uint32_t get32(const uint8_t* bytes) {
    return static_cast<uint32_t>(bytes[0]) << 24 | bytes[1] << 16 | bytes[2] << 8 | bytes[3];
}

uint16_t get16(const uint8_t* bytes) {
    return static_cast<uint16_t>(bytes[0] << 8 | bytes[1]);
}

bool read_all(int fd, void* buffer, size_t bytes) {
    auto* p = static_cast<uint8_t*>(buffer);
    while (bytes > 0) {
        const ssize_t n = recv(fd, p, bytes, 0);
        if (n <= 0) return false;
        p += n;
        bytes -= static_cast<size_t>(n);
    }
    return true;
}

bool write_all(int fd, const uint8_t* data, size_t bytes) {
    while (bytes > 0) {
        const ssize_t n = send(fd, data, bytes, 0);
        if (n <= 0) return false;
        data += n;
        bytes -= static_cast<size_t>(n);
    }
    return true;
}

struct Urb {
    uint32_t seqnum;
    uint32_t length;
    uint32_t flags;
    std::vector<uint8_t> data;
    uint32_t done = 0;
};

struct IsoPacket {
    uint32_t offset;
    uint32_t length;
    uint32_t actual;
};

struct Ep {
    bool open = false;
    bool in = false;
    bool stalled = false;
    bool iso = false;
    bool zlp_pending = false;
    uint16_t max_packet_bytes = 0;
    uint32_t offset = 0;
    std::deque<Transfer*> transfers;
    std::deque<Urb> urbs;
};

}  // namespace

struct Dcd::State {
    Port port = Port::HighSpeed;
    QueueHandle_t events = nullptr;
    const Descriptors* descriptors = nullptr;

    std::mutex lock;
    std::condition_variable outbox_ready;
    std::deque<std::vector<uint8_t>> outbox;
    int listener = -1;
    int client = -1;
    bool stopping = false;
    std::thread reader;
    std::thread writer;

    std::vector<uint8_t> ep0_buffer;
    bool ep0_pending = false;
    bool ep0_in = false;
    uint32_t ep0_seqnum = 0;
    uint32_t ep0_length = 0;
    std::vector<uint8_t> ep0_data;

    Ep in_eps[kMaxEndpoints];
    Ep out_eps[kMaxEndpoints];

    std::deque<Transfer*> done;
    bool done_posted = false;
    std::vector<Event> pending_events;

    void reader_main();
    void writer_main();
    void serve(int fd);
    bool handle_command(int fd, const uint8_t* header);
    void disconnect();

    void post(EventType type, const SetupPacket* setup = nullptr, uint16_t length = 0);
    void flush_events(TickType_t wait);
    void reply(uint32_t seqnum, int32_t status, uint32_t actual, const uint8_t* data,
               size_t bytes);
    void reply_unlink(uint32_t seqnum, int32_t status);
    void reply_iso(uint32_t seqnum, const std::vector<uint8_t>& data,
                   const std::vector<IsoPacket>& packets);
    void process_iso(Ep& ep, uint32_t seqnum, const std::vector<uint8_t>& data,
                     std::vector<IsoPacket>& packets);
    std::vector<uint8_t> device_record() const;

    Ep* endpoint(uint8_t address);
    void complete(Transfer* transfer, TransferStatus status, uint32_t actual);
    void cancel_transfers(Ep& ep, TransferStatus status);
    void fail_urbs(Ep& ep, int32_t status);
    void process(Ep& ep);
    void process_out(Ep& ep);
    void process_in(Ep& ep);
};

void Dcd::State::post(EventType type, const SetupPacket* setup, uint16_t length) {
    Event event = {};
    event.type = type;
    event.speed = port == Port::HighSpeed ? Speed::High : Speed::Full;
    event.length = length;
    if (setup) event.setup = *setup;
    pending_events.push_back(event);
}

// Called without the lock: the device task takes it to answer these events.
void Dcd::State::flush_events(TickType_t wait) {
    std::vector<Event> events_out;
    {
        std::lock_guard<std::mutex> guard(lock);
        events_out.swap(pending_events);
    }
    for (const Event& event : events_out) {
        if (xQueueSend(events, &event, wait) != pdTRUE) ESP_LOGE(TAG, "event dropped");
    }
}

void Dcd::State::reply(uint32_t seqnum, int32_t status, uint32_t actual, const uint8_t* data,
                       size_t bytes) {
    if (client < 0) return;
    std::vector<uint8_t> out;
    out.reserve(kHeaderBytes + bytes);
    put32(out, kRetSubmit);
    put32(out, seqnum);
    put32(out, 0);
    put32(out, 0);
    put32(out, 0);
    put32(out, static_cast<uint32_t>(status));
    put32(out, actual);
    put32(out, 0);
    put32(out, 0);
    put32(out, 0);
    out.insert(out.end(), 8, 0);
    if (data && bytes) out.insert(out.end(), data, data + bytes);
    outbox.push_back(std::move(out));
    outbox_ready.notify_one();
}

// Isochronous IN data goes back packed, each packet's actual bytes after the
// last, as the Linux USB/IP server sends it.
void Dcd::State::reply_iso(uint32_t seqnum, const std::vector<uint8_t>& data,
                           const std::vector<IsoPacket>& packets) {
    if (client < 0) return;
    uint32_t actual = 0;
    for (const IsoPacket& packet : packets) actual += packet.actual;
    std::vector<uint8_t> out;
    put32(out, kRetSubmit);
    put32(out, seqnum);
    put32(out, 0);
    put32(out, 0);
    put32(out, 0);
    put32(out, 0);
    put32(out, actual);
    put32(out, 0);
    put32(out, static_cast<uint32_t>(packets.size()));
    put32(out, 0);
    out.insert(out.end(), 8, 0);
    out.insert(out.end(), data.begin(), data.end());
    for (const IsoPacket& packet : packets) {
        put32(out, packet.offset);
        put32(out, packet.length);
        put32(out, packet.actual);
        put32(out, 0);
    }
    outbox.push_back(std::move(out));
    outbox_ready.notify_one();
}

// Each packet of an isochronous URB is one device transfer. With none queued
// an OUT packet is lost and an IN packet goes out empty, as on the bus.
void Dcd::State::process_iso(Ep& ep, uint32_t seqnum, const std::vector<uint8_t>& data,
                             std::vector<IsoPacket>& packets) {
    std::vector<uint8_t> in_data;
    for (IsoPacket& packet : packets) {
        packet.actual = 0;
        if (ep.transfers.empty()) continue;
        Transfer* transfer = ep.transfers.front();
        ep.transfers.pop_front();
        if (ep.in) {
            packet.actual = std::min(packet.length, transfer->length);
            in_data.insert(in_data.end(), transfer->buffer, transfer->buffer + packet.actual);
        } else if (packet.offset + packet.length <= data.size()) {
            packet.actual = std::min(packet.length, transfer->length);
            memcpy(transfer->buffer, data.data() + packet.offset, packet.actual);
        }
        complete(transfer, TransferStatus::Completed, packet.actual);
    }
    reply_iso(seqnum, in_data, packets);
}

void Dcd::State::reply_unlink(uint32_t seqnum, int32_t status) {
    if (client < 0) return;
    std::vector<uint8_t> out;
    put32(out, kRetUnlink);
    put32(out, seqnum);
    put32(out, 0);
    put32(out, 0);
    put32(out, 0);
    put32(out, static_cast<uint32_t>(status));
    out.insert(out.end(), kHeaderBytes - out.size(), 0);
    outbox.push_back(std::move(out));
    outbox_ready.notify_one();
}

std::vector<uint8_t> Dcd::State::device_record() const {
    const std::vector<uint8_t>& d = descriptors->device;
    const std::vector<uint8_t>& config =
        descriptors->config_for(port == Port::HighSpeed ? Speed::High : Speed::Full);
    std::vector<uint8_t> out;
    put_string(out, kPath, kPathBytes);
    put_string(out, kBusId, kBusIdBytes);
    put32(out, kBusNum);
    put32(out, kDevNum);
    put32(out, port == Port::HighSpeed ? kSpeedHigh : kSpeedFull);
    put16(out, static_cast<uint16_t>(d[8] | d[9] << 8));
    put16(out, static_cast<uint16_t>(d[10] | d[11] << 8));
    put16(out, static_cast<uint16_t>(d[12] | d[13] << 8));
    put8(out, d[4]);
    put8(out, d[5]);
    put8(out, d[6]);
    put8(out, 0);
    put8(out, d[17]);
    put8(out, config.size() > 4 ? config[4] : 0);
    return out;
}

Ep* Dcd::State::endpoint(uint8_t address) {
    const uint8_t number = address & 0x7f;
    if (number == 0 || number >= kMaxEndpoints) return nullptr;
    return (address & 0x80) ? &in_eps[number] : &out_eps[number];
}

void Dcd::State::complete(Transfer* transfer, TransferStatus status, uint32_t actual) {
    transfer->status = status;
    transfer->actual = actual;
    done.push_back(transfer);
    if (!done_posted) {
        done_posted = true;
        post(EventType::TransfersDone);
    }
}

void Dcd::State::cancel_transfers(Ep& ep, TransferStatus status) {
    bool front = true;
    for (Transfer* transfer : ep.transfers) {
        complete(transfer, status, front ? ep.offset : 0);
        front = false;
    }
    ep.transfers.clear();
    ep.offset = 0;
    ep.zlp_pending = false;
}

void Dcd::State::fail_urbs(Ep& ep, int32_t status) {
    for (const Urb& urb : ep.urbs) reply(urb.seqnum, status, 0, nullptr, 0);
    ep.urbs.clear();
}

void Dcd::State::process(Ep& ep) {
    if (!ep.open || ep.stalled) return;
    if (ep.in) {
        process_in(ep);
    } else {
        process_out(ep);
    }
}

// A host URB is a run of packets: one whose length is not a multiple of the
// packet size, or that asks for a zero-length packet, ends the device
// transfer it lands in, as a short packet does on the bus.
void Dcd::State::process_out(Ep& ep) {
    while (!ep.transfers.empty()) {
        Transfer* transfer = ep.transfers.front();
        if (ep.zlp_pending) {
            ep.zlp_pending = false;
            ep.transfers.pop_front();
            complete(transfer, TransferStatus::Completed, 0);
            continue;
        }
        if (ep.urbs.empty()) break;
        Urb& urb = ep.urbs.front();
        const uint32_t bytes = std::min(urb.length - urb.done, transfer->length - ep.offset);
        memcpy(transfer->buffer + ep.offset, urb.data.data() + urb.done, bytes);
        ep.offset += bytes;
        urb.done += bytes;

        const bool urb_done = urb.done == urb.length;
        const bool full = ep.offset == transfer->length;
        bool ends = false;
        if (urb_done) {
            const bool zlp = urb.length % ep.max_packet_bytes == 0 && (urb.flags & kUrbZeroPacket);
            ends = urb.length % ep.max_packet_bytes != 0 || urb.length == 0 || zlp;
            if (zlp && full) ep.zlp_pending = true;
            reply(urb.seqnum, 0, urb.length, nullptr, 0);
            ep.urbs.pop_front();
        }
        if (ends || full) {
            ep.transfers.pop_front();
            complete(transfer, TransferStatus::Completed, ep.offset);
            ep.offset = 0;
        }
    }
}

void Dcd::State::process_in(Ep& ep) {
    while (!ep.urbs.empty()) {
        Urb& urb = ep.urbs.front();
        if (ep.zlp_pending) {
            ep.zlp_pending = false;
            reply(urb.seqnum, 0, urb.done, urb.data.data(), urb.done);
            ep.urbs.pop_front();
            continue;
        }
        if (ep.transfers.empty()) break;
        Transfer* transfer = ep.transfers.front();
        const uint32_t bytes = std::min(urb.length - urb.done, transfer->length - ep.offset);
        urb.data.insert(urb.data.end(), transfer->buffer + ep.offset,
                        transfer->buffer + ep.offset + bytes);
        urb.done += bytes;
        ep.offset += bytes;

        bool ends = false;
        if (ep.offset == transfer->length) {
            const bool whole = transfer->length % ep.max_packet_bytes == 0;
            ends = !whole || transfer->zlp || transfer->length == 0;
            if (whole && transfer->zlp && transfer->length > 0 && urb.done == urb.length) {
                ep.zlp_pending = true;
                ends = false;
            }
            ep.transfers.pop_front();
            complete(transfer, TransferStatus::Completed, transfer->length);
            ep.offset = 0;
        }
        if (ends || urb.done == urb.length) {
            reply(urb.seqnum, 0, urb.done, urb.data.data(), urb.done);
            ep.urbs.pop_front();
        }
    }
}

bool Dcd::State::handle_command(int fd, const uint8_t* header) {
    const uint32_t command = get32(header);
    const uint32_t seqnum = get32(header + 4);
    const uint32_t direction = get32(header + 12);
    const uint32_t number = get32(header + 16);

    if (command == kCmdUnlink) {
        const uint32_t target = get32(header + 20);
        std::lock_guard<std::mutex> guard(lock);
        bool found = false;
        for (Ep* eps : {in_eps, out_eps}) {
            for (int n = 0; n < kMaxEndpoints && !found; n++) {
                auto& urbs = eps[n].urbs;
                auto it = std::find_if(urbs.begin(), urbs.end(),
                                       [&](const Urb& u) { return u.seqnum == target; });
                if (it == urbs.end()) continue;
                urbs.erase(it);
                found = true;
            }
        }
        if (!found && ep0_pending && ep0_seqnum == target) {
            ep0_pending = false;
            found = true;
        }
        reply_unlink(seqnum, found ? kErrConnReset : 0);
        return true;
    }
    if (command != kCmdSubmit) return false;

    const uint32_t flags = get32(header + 20);
    const uint32_t length = get32(header + 24);
    const uint32_t packets = get32(header + 32);
    std::vector<uint8_t> data;
    if (direction != kDirIn && length > 0) {
        data.resize(length);
        if (!read_all(fd, data.data(), length)) return false;
    }
    if (packets != 0 && packets != kNoIsoPackets) {
        std::vector<uint8_t> raw(16 * static_cast<size_t>(packets));
        if (!read_all(fd, raw.data(), raw.size())) return false;
        std::vector<IsoPacket> iso(packets);
        for (uint32_t i = 0; i < packets; i++) {
            iso[i] = {get32(&raw[16 * i]), get32(&raw[16 * i + 4]), 0};
        }
        std::lock_guard<std::mutex> guard(lock);
        Ep* ep = endpoint(static_cast<uint8_t>(number | (direction == kDirIn ? 0x80 : 0)));
        if (!ep || !ep->open || !ep->iso) {
            reply(seqnum, kErrPipe, 0, nullptr, 0);
        } else {
            process_iso(*ep, seqnum, data, iso);
        }
        return true;
    }

    std::lock_guard<std::mutex> guard(lock);
    if (number == 0) {
        SetupPacket setup;
        memcpy(&setup, header + 40, sizeof(setup));
        ep0_pending = true;
        ep0_in = direction == kDirIn;
        ep0_seqnum = seqnum;
        ep0_length = length;
        ep0_data = std::move(data);
        post(EventType::Setup, &setup);
        return true;
    }
    Ep* ep = endpoint(static_cast<uint8_t>(number | (direction == kDirIn ? 0x80 : 0)));
    if (!ep || !ep->open || ep->stalled) {
        reply(seqnum, kErrPipe, 0, nullptr, 0);
        return true;
    }
    Urb urb;
    urb.seqnum = seqnum;
    urb.length = length;
    urb.flags = flags;
    urb.data = std::move(data);
    ep->urbs.push_back(std::move(urb));
    process(*ep);
    return true;
}

void Dcd::State::disconnect() {
    {
        std::lock_guard<std::mutex> guard(lock);
        ep0_pending = false;
        for (int n = 1; n < kMaxEndpoints; n++) {
            for (Ep* ep : {&in_eps[n], &out_eps[n]}) {
                ep->urbs.clear();
                if (!ep->open) continue;
                cancel_transfers(*ep, TransferStatus::Canceled);
                ep->open = false;
                ep->stalled = false;
            }
        }
        outbox.clear();
        client = -1;
        post(EventType::Reset);
    }
    flush_events(portMAX_DELAY);
}

void Dcd::State::serve(int fd) {
    uint8_t op[8];
    if (!read_all(fd, op, sizeof(op)) || get16(op) != kUsbipVersion) return;
    const uint16_t code = get16(op + 2);

    if (code == kOpReqDevlist) {
        std::vector<uint8_t> out;
        put16(out, kUsbipVersion);
        put16(out, kOpRepDevlist);
        put32(out, 0);
        put32(out, 1);
        const std::vector<uint8_t> record = device_record();
        out.insert(out.end(), record.begin(), record.end());
        const std::vector<uint8_t>& config =
            descriptors->config_for(port == Port::HighSpeed ? Speed::High : Speed::Full);
        for (size_t pos = 0; pos + 8 < config.size() && config[pos] > 0; pos += config[pos]) {
            if (config[pos + 1] != kDescInterface || config[pos + 3] != 0) continue;
            out.insert(out.end(), {config[pos + 5], config[pos + 6], config[pos + 7], 0});
        }
        write_all(fd, out.data(), out.size());
        return;
    }
    if (code != kOpReqImport) return;

    char busid[kBusIdBytes + 1] = {};
    if (!read_all(fd, busid, kBusIdBytes)) return;
    const bool match = strcmp(busid, kBusId) == 0;
    std::vector<uint8_t> out;
    put16(out, kUsbipVersion);
    put16(out, kOpRepImport);
    put32(out, match ? 0 : 1);
    if (match) {
        const std::vector<uint8_t> record = device_record();
        out.insert(out.end(), record.begin(), record.end());
    }
    if (!write_all(fd, out.data(), out.size()) || !match) return;

    ESP_LOGI(TAG, "usbip client attached");
    {
        std::lock_guard<std::mutex> guard(lock);
        client = fd;
        post(EventType::Reset);
        post(EventType::Enumerated);
    }
    flush_events(portMAX_DELAY);

    uint8_t header[kHeaderBytes];
    while (read_all(fd, header, sizeof(header))) {
        if (!handle_command(fd, header)) break;
        flush_events(portMAX_DELAY);
    }
    disconnect();
    ESP_LOGI(TAG, "usbip client detached");
}

void Dcd::State::reader_main() {
    while (true) {
        const int fd = accept(listener, nullptr, nullptr);
        if (fd < 0) return;
        const int one = 1;
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
        serve(fd);
        shutdown(fd, SHUT_RDWR);
        close(fd);
        std::lock_guard<std::mutex> guard(lock);
        if (stopping) return;
    }
}

void Dcd::State::writer_main() {
    std::unique_lock<std::mutex> guard(lock);
    while (true) {
        outbox_ready.wait(guard, [this] { return stopping || !outbox.empty(); });
        if (stopping) return;
        std::vector<uint8_t> out = std::move(outbox.front());
        outbox.pop_front();
        const int fd = client;
        guard.unlock();
        if (fd >= 0) write_all(fd, out.data(), out.size());
        guard.lock();
    }
}

Dcd::Dcd() = default;

Dcd::~Dcd() {
    stop();
}

esp_err_t Dcd::start(Port port, size_t ep0_bytes, QueueHandle_t events,
                     const Descriptors& descriptors) {
    if (state_) return ESP_ERR_INVALID_STATE;

    const char* listen_env = getenv(kListenEnv);
    const std::string listen_at = listen_env ? listen_env : kDefaultListen;
    const size_t colon = listen_at.rfind(':');
    sockaddr_in address = {};
    address.sin_family = AF_INET;
    if (colon == std::string::npos ||
        inet_pton(AF_INET, listen_at.substr(0, colon).c_str(), &address.sin_addr) != 1) {
        ESP_LOGE(TAG, "%s: expected <ipv4>:<port>, got %s", kListenEnv, listen_at.c_str());
        return ESP_ERR_INVALID_ARG;
    }
    address.sin_port = htons(static_cast<uint16_t>(atoi(listen_at.c_str() + colon + 1)));

    const int listener = socket(AF_INET, SOCK_STREAM, 0);
    const int one = 1;
    setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    if (listener < 0 || bind(listener, reinterpret_cast<sockaddr*>(&address), sizeof(address)) ||
        listen(listener, 1)) {
        ESP_LOGE(TAG, "usbip listen on %s: %s", listen_at.c_str(), strerror(errno));
        if (listener >= 0) close(listener);
        return ESP_FAIL;
    }

    auto* s = new State();
    s->port = port;
    s->events = events;
    s->descriptors = &descriptors;
    s->listener = listener;
    s->ep0_buffer.resize(ep0_bytes);
    for (int n = 0; n < kMaxEndpoints; n++) s->in_eps[n].in = true;
    s->reader = std::thread([s] { s->reader_main(); });
    s->writer = std::thread([s] { s->writer_main(); });
    state_ = s;
    ESP_LOGI(TAG, "usbip server on %s, bus id %s", listen_at.c_str(), kBusId);
    return ESP_OK;
}

void Dcd::stop() {
    if (!state_) return;
    State* s = state_;
    {
        std::lock_guard<std::mutex> guard(s->lock);
        s->stopping = true;
        if (s->client >= 0) shutdown(s->client, SHUT_RDWR);
        s->outbox_ready.notify_all();
    }
    shutdown(s->listener, SHUT_RDWR);
    close(s->listener);
    s->reader.join();
    s->writer.join();
    delete s;
    state_ = nullptr;
}

uint8_t* Dcd::ep0_buffer() {
    return state_->ep0_buffer.data();
}

void Dcd::ep0_send(size_t bytes, bool) {
    std::lock_guard<std::mutex> guard(state_->lock);
    State& s = *state_;
    if (!s.ep0_pending || !s.ep0_in) return;
    s.ep0_pending = false;
    const size_t length = std::min<size_t>(bytes, s.ep0_length);
    s.reply(s.ep0_seqnum, 0, static_cast<uint32_t>(length), s.ep0_buffer.data(), length);
}

void Dcd::ep0_receive(size_t bytes) {
    {
        std::lock_guard<std::mutex> guard(state_->lock);
        State& s = *state_;
        if (!s.ep0_pending || s.ep0_in) return;
        const size_t length = std::min({bytes, s.ep0_data.size(), s.ep0_buffer.size()});
        memcpy(s.ep0_buffer.data(), s.ep0_data.data(), length);
        s.post(EventType::Ep0Received, nullptr, static_cast<uint16_t>(length));
    }
    state_->flush_events(0);
}

void Dcd::ep0_ack() {
    std::lock_guard<std::mutex> guard(state_->lock);
    State& s = *state_;
    if (!s.ep0_pending) return;
    s.ep0_pending = false;
    const uint32_t actual = s.ep0_in ? 0 : static_cast<uint32_t>(s.ep0_data.size());
    s.reply(s.ep0_seqnum, 0, actual, nullptr, 0);
}

void Dcd::ep0_stall() {
    std::lock_guard<std::mutex> guard(state_->lock);
    State& s = *state_;
    if (!s.ep0_pending) return;
    s.ep0_pending = false;
    s.reply(s.ep0_seqnum, kErrPipe, 0, nullptr, 0);
}

void Dcd::set_address(uint8_t) {}

esp_err_t Dcd::open_endpoint(uint8_t address, EndpointType type, uint16_t max_packet_bytes,
                             uint8_t) {
    if (!state_) return ESP_ERR_INVALID_STATE;
    if (type == EndpointType::Control) return ESP_ERR_NOT_SUPPORTED;
    std::lock_guard<std::mutex> guard(state_->lock);
    Ep* ep = state_->endpoint(address);
    if (!ep || max_packet_bytes == 0) return ESP_ERR_INVALID_ARG;
    if (ep->open) return ESP_ERR_INVALID_STATE;
    ep->open = true;
    ep->stalled = false;
    ep->iso = type == EndpointType::Isochronous;
    ep->zlp_pending = false;
    ep->offset = 0;
    ep->max_packet_bytes = max_packet_bytes;
    return ESP_OK;
}

void Dcd::close_endpoint(uint8_t address) {
    if (!state_) return;
    {
        std::lock_guard<std::mutex> guard(state_->lock);
        Ep* ep = state_->endpoint(address);
        if (!ep || !ep->open) return;
        state_->cancel_transfers(*ep, TransferStatus::Canceled);
        state_->fail_urbs(*ep, kErrShutdown);
        ep->open = false;
        ep->stalled = false;
    }
    state_->flush_events(0);
}

esp_err_t Dcd::submit(Transfer* transfer) {
    if (!state_) return ESP_ERR_INVALID_STATE;
    {
        std::lock_guard<std::mutex> guard(state_->lock);
        Ep* ep = state_->endpoint(transfer->endpoint);
        if (!ep) return ESP_ERR_INVALID_ARG;
        if (!ep->open) return ESP_ERR_INVALID_STATE;
        if (!ep->iso && !ep->in &&
            (transfer->length == 0 || transfer->length % ep->max_packet_bytes != 0)) {
            return ESP_ERR_INVALID_SIZE;
        }
        transfer->actual = 0;
        transfer->status = TransferStatus::Completed;
        ep->transfers.push_back(transfer);
        state_->process(*ep);
    }
    state_->flush_events(0);
    return ESP_OK;
}

void Dcd::flush(uint8_t address) {
    if (!state_) return;
    {
        std::lock_guard<std::mutex> guard(state_->lock);
        Ep* ep = state_->endpoint(address);
        if (!ep || !ep->open) return;
        state_->cancel_transfers(*ep, TransferStatus::Canceled);
    }
    state_->flush_events(0);
}

esp_err_t Dcd::set_stall(uint8_t address, bool stall) {
    if (!state_) return ESP_ERR_INVALID_STATE;
    {
        std::lock_guard<std::mutex> guard(state_->lock);
        Ep* ep = state_->endpoint(address);
        if (!ep) return ESP_ERR_INVALID_ARG;
        if (!ep->open) return ESP_ERR_INVALID_STATE;
        ep->stalled = stall;
        if (stall) {
            state_->fail_urbs(*ep, kErrPipe);
            if (ep->in && !ep->transfers.empty()) {
                Transfer* transfer = ep->transfers.front();
                ep->transfers.pop_front();
                state_->complete(transfer, TransferStatus::Stalled, ep->offset);
                ep->offset = 0;
            }
        } else {
            state_->process(*ep);
        }
    }
    state_->flush_events(0);
    return ESP_OK;
}

bool Dcd::stalled(uint8_t address) {
    if (!state_) return false;
    std::lock_guard<std::mutex> guard(state_->lock);
    Ep* ep = state_->endpoint(address);
    return ep && ep->stalled;
}

Transfer* Dcd::take_done() {
    if (!state_) return nullptr;
    std::lock_guard<std::mutex> guard(state_->lock);
    if (state_->done.empty()) {
        state_->done_posted = false;
        return nullptr;
    }
    Transfer* transfer = state_->done.front();
    state_->done.pop_front();
    return transfer;
}

}  // namespace usb_device::detail
