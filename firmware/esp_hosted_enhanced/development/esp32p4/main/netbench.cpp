/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "netbench.hpp"

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstring>

#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lwip/sockets.h"

namespace {

const char* TAG = "netbench";
constexpr size_t kBufSize = 16 * 1024;

std::atomic<bool> s_started{false};

uint8_t* alloc_buf() {
    auto* buf = static_cast<uint8_t*>(heap_caps_malloc(kBufSize, MALLOC_CAP_SPIRAM));
    assert(buf);
    return buf;
}

int listen_tcp(int port) {
    int fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    sockaddr_in addr = {};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0 ||
        listen(fd, 1) != 0) {
        ESP_LOGE(TAG, "listen %d failed: %d", port, errno);
        close(fd);
        return -1;
    }
    return fd;
}

void sink_task(void*) {
    uint8_t* buf = alloc_buf();
    int server = listen_tcp(kNetbenchSinkPort);
    for (;;) {
        int fd = accept(server, nullptr, nullptr);
        if (fd < 0) continue;
        size_t total = 0;
        for (;;) {
            int n = recv(fd, buf, kBufSize, 0);
            if (n <= 0) break;
            total += n;
        }
        close(fd);
        ESP_LOGI(TAG, "sink: %u bytes", (unsigned)total);
    }
}

void source_task(void*) {
    uint8_t* buf = alloc_buf();
    for (size_t i = 0; i < kBufSize; i++) buf[i] = (uint8_t)i;
    int server = listen_tcp(kNetbenchSourcePort);
    for (;;) {
        int fd = accept(server, nullptr, nullptr);
        if (fd < 0) continue;
        size_t total = 0;
        for (;;) {
            int n = send(fd, buf, kBufSize, 0);
            if (n <= 0) break;
            total += n;
        }
        close(fd);
        ESP_LOGI(TAG, "source: %u bytes", (unsigned)total);
    }
}

void echo_task(void*) {
    uint8_t* buf = alloc_buf();
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    sockaddr_in addr = {};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(kNetbenchEchoPort);
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr));
    for (;;) {
        sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        int n = recvfrom(fd, buf, kBufSize, 0, reinterpret_cast<sockaddr*>(&peer), &peer_len);
        if (n > 0) {
            sendto(fd, buf, n, 0, reinterpret_cast<sockaddr*>(&peer), peer_len);
        }
    }
}

enum : uint8_t { kUdpData, kUdpReset, kUdpReport, kUdpSend };

struct __attribute__((packed)) UdpHdr {
    uint8_t cmd;
    uint8_t reserved[3];
    uint32_t arg[3];
};

int bind_udp(int port) {
    int fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    sockaddr_in addr = {};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr));
    return fd;
}

void udp_reply(int fd, const sockaddr_in& peer, uint8_t cmd, uint32_t a0, uint32_t a1, uint32_t a2) {
    UdpHdr h = {};
    h.cmd = cmd;
    h.arg[0] = a0;
    h.arg[1] = a1;
    h.arg[2] = a2;
    sendto(fd, &h, sizeof(h), 0, reinterpret_cast<const sockaddr*>(&peer), sizeof(peer));
}

/* arg0: duration (ms), arg1: datagram size, arg2: rate (kbit/s, 0: as fast as possible) */
void udp_send(int fd, uint8_t* buf, const sockaddr_in& peer, const UdpHdr& req) {
    size_t size = std::min<size_t>(std::max<size_t>(req.arg[1], sizeof(UdpHdr)), kBufSize);
    memset(buf, 0, size);
    auto* h = reinterpret_cast<UdpHdr*>(buf);
    h->cmd = kUdpData;
    int64_t t0 = esp_timer_get_time();
    int64_t end = t0 + (int64_t)req.arg[0] * 1000;
    uint32_t sent = 0, errors = 0;
    for (int64_t now = t0; now < end; now = esp_timer_get_time()) {
        if (req.arg[2]) {
            int64_t due = t0 + (int64_t)sent * size * 8000 / req.arg[2];
            if (due > now + 2000) {
                vTaskDelay(pdMS_TO_TICKS((due - now) / 1000));
                continue;
            }
        }
        h->arg[0] = sent;
        if (sendto(fd, buf, size, 0, reinterpret_cast<const sockaddr*>(&peer), sizeof(peer)) == (int)size) {
            sent++;
        } else {
            errors++;
            vTaskDelay(1);
        }
    }
    for (int i = 0; i < 5; i++) {
        vTaskDelay(pdMS_TO_TICKS(50));
        udp_reply(fd, peer, kUdpReport, sent, errors, esp_timer_get_time() - t0);
    }
}

void udp_task(void*) {
    uint8_t* buf = alloc_buf();
    int fd = bind_udp(kNetbenchUdpPort);
    uint32_t packets = 0, bytes = 0;
    int64_t first_us = 0, last_us = 0;
    for (;;) {
        sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        int n = recvfrom(fd, buf, kBufSize, 0, reinterpret_cast<sockaddr*>(&peer), &peer_len);
        if (n < (int)sizeof(UdpHdr)) continue;
        UdpHdr h;
        memcpy(&h, buf, sizeof(h));
        switch (h.cmd) {
        case kUdpData:
            last_us = esp_timer_get_time();
            if (packets++ == 0) first_us = last_us;
            bytes += n;
            break;
        case kUdpReset:
            packets = bytes = 0;
            break;
        case kUdpReport:
            udp_reply(fd, peer, kUdpReport, packets, bytes, last_us - first_us);
            break;
        case kUdpSend:
            udp_send(fd, buf, peer, h);
            break;
        }
    }
}

}  // namespace

void netbench_start() {
    if (s_started.exchange(true)) return;
    xTaskCreate(sink_task, "nb_sink", 4096, nullptr, 5, nullptr);
    xTaskCreate(source_task, "nb_source", 4096, nullptr, 5, nullptr);
    xTaskCreate(echo_task, "nb_echo", 4096, nullptr, 6, nullptr);
    xTaskCreate(udp_task, "nb_udp", 4096, nullptr, 5, nullptr);
}
