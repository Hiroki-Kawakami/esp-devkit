/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "netbench.hpp"

#include <atomic>
#include <cstdint>
#include <cstring>

#include "esp_heap_caps.h"
#include "esp_log.h"
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

}  // namespace

void netbench_start() {
    if (s_started.exchange(true)) return;
    xTaskCreate(sink_task, "nb_sink", 4096, nullptr, 5, nullptr);
    xTaskCreate(source_task, "nb_source", 4096, nullptr, 5, nullptr);
    xTaskCreate(echo_task, "nb_echo", 4096, nullptr, 6, nullptr);
}
