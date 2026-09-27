/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sdio_bench.hpp"

#include "sdkconfig.h"

#if CONFIG_ESP_HOSTED_ENHANCED_HOST

#include <algorithm>
#include <atomic>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "esp_rom_sys.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "harness.h"
#include "hosted_host.h"
#include "hosted_wire.h"
#include "sd_protocol_defs.h"

namespace {

constexpr uint32_t kWaitMs = 5000;

SemaphoreHandle_t s_event;
std::atomic<uint32_t> s_rx_packets;
std::atomic<uint32_t> s_rx_bytes;
std::atomic<int64_t> s_first_us;
std::atomic<int64_t> s_last_us;
std::atomic<uint32_t> s_stats_bytes;
std::atomic<int64_t> s_echo_us;

uint8_t s_pkt[HOSTED_MAX_PAYLOAD];

void on_test(const uint8_t* data, size_t len, void*) {
    if (len < sizeof(hosted_test_hdr_t)) return;
    hosted_test_hdr_t h;
    memcpy(&h, data, sizeof(h));
    int64_t now = esp_timer_get_time();
    if (h.cmd == HOSTED_TEST_STATS) {
        s_stats_bytes = h.arg[1];
        xSemaphoreGive(s_event);
    } else if (h.cmd == HOSTED_TEST_DATA) {
        if (s_rx_packets++ == 0) s_first_us = now;
        s_last_us = now;
        s_rx_bytes += len;
        s_echo_us = now - ((int64_t)h.arg[1] << 32 | h.arg[0]);
        xSemaphoreGive(s_event);
    }
}

esp_err_t control(hosted_test_cmd_t cmd, uint32_t a0 = 0, uint32_t a1 = 0) {
    hosted_test_hdr_t h = {};
    h.cmd = cmd;
    h.arg[0] = a0;
    h.arg[1] = a1;
    return hosted_host_test_send(&h, sizeof(h), kWaitMs);
}

size_t clamp_size(int size) {
    return std::clamp<size_t>(size, sizeof(hosted_test_hdr_t), HOSTED_MAX_PAYLOAD);
}

bool reply_err(const char* what, esp_err_t err) {
    harness_reply("ERR sdio %s: %s", what, esp_err_to_name(err));
    return true;
}

bool sink(size_t size, uint32_t count, uint32_t delay_us) {
    esp_err_t err = control(HOSTED_TEST_SET_MODE, HOSTED_TEST_SINK, delay_us);
    if (err != ESP_OK) return reply_err("sink", err);
    auto* h = reinterpret_cast<hosted_test_hdr_t*>(s_pkt);
    memset(h, 0, sizeof(*h));
    h->cmd = HOSTED_TEST_DATA;
    xSemaphoreTake(s_event, 0);
    int64_t t0 = esp_timer_get_time();
    for (uint32_t i = 0; i < count && err == ESP_OK; i++) {
        h->seq = i;
        err = hosted_host_test_send(s_pkt, size, kWaitMs);
    }
    if (err == ESP_OK) err = control(HOSTED_TEST_STATS);
    if (err == ESP_OK && xSemaphoreTake(s_event, pdMS_TO_TICKS(kWaitMs)) != pdTRUE) err = ESP_ERR_TIMEOUT;
    if (err != ESP_OK) return reply_err("sink", err);
    harness_reply("OK sdio sink %u %u", (unsigned)s_stats_bytes.load(),
                  (unsigned)(esp_timer_get_time() - t0));
    return true;
}

bool source(size_t size, uint32_t count) {
    esp_err_t err = control(HOSTED_TEST_SET_MODE, HOSTED_TEST_SINK, 0);
    s_rx_packets = 0;
    s_rx_bytes = 0;
    if (err == ESP_OK) err = control(HOSTED_TEST_SOURCE, size, count);
    if (err != ESP_OK) return reply_err("source", err);
    uint32_t seen = 0;
    while (s_rx_packets < count) {
        vTaskDelay(pdMS_TO_TICKS(200));
        if (s_rx_packets == seen) break;
        seen = s_rx_packets;
    }
    harness_reply("OK sdio source %u %u %u", (unsigned)s_rx_bytes.load(),
                  (unsigned)(s_last_us - s_first_us), (unsigned)(count - s_rx_packets.load()));
    return true;
}

bool echo(size_t size, uint32_t count, uint32_t gap_us) {
    esp_err_t err = control(HOSTED_TEST_SET_MODE, HOSTED_TEST_ECHO, 0);
    if (err != ESP_OK) return reply_err("echo", err);
    auto* h = reinterpret_cast<hosted_test_hdr_t*>(s_pkt);
    memset(h, 0, sizeof(*h));
    h->cmd = HOSTED_TEST_DATA;
    std::vector<uint32_t> rtts;
    rtts.reserve(count);
    for (uint32_t i = 0; i < count; i++) {
        xSemaphoreTake(s_event, 0);
        int64_t now = esp_timer_get_time();
        h->seq = i;
        h->arg[0] = static_cast<uint32_t>(now);
        h->arg[1] = static_cast<uint32_t>(now >> 32);
        if (hosted_host_test_send(s_pkt, size, kWaitMs) != ESP_OK) continue;
        if (xSemaphoreTake(s_event, pdMS_TO_TICKS(100)) == pdTRUE) rtts.push_back(s_echo_us);
        if (gap_us) esp_rom_delay_us(gap_us);
    }
    control(HOSTED_TEST_SET_MODE, HOSTED_TEST_SINK, 0);
    if (rtts.empty()) return reply_err("echo", ESP_ERR_TIMEOUT);
    std::sort(rtts.begin(), rtts.end());
    harness_reply("OK sdio echo %u %u %u %u", (unsigned)rtts[rtts.size() / 2],
                  (unsigned)rtts[std::min(rtts.size() - 1, rtts.size() * 99 / 100)],
                  (unsigned)rtts.back(), (unsigned)(count - rtts.size()));
    return true;
}

struct ShareJob {
    uint32_t count;
    std::atomic<uint32_t> ok;
    std::atomic<uint32_t> err;
    SemaphoreHandle_t done;
};

void share_task(void* arg) {
    auto* job = static_cast<ShareJob*>(arg);
    for (uint32_t i = 0; i < job->count; i++) {
        sdmmc_command_t cmd = {};
        cmd.opcode = SD_IO_RW_DIRECT;
        cmd.flags = SCF_CMD_AC | SCF_RSP_R5;
        cmd.timeout_ms = 1000;
        esp_err_t err = hosted_host_sdmmc_do_transaction(CONFIG_ESP_HOSTED_ENHANCED_SDIO_SLOT, &cmd);
        (err == ESP_OK ? job->ok : job->err)++;
    }
    xSemaphoreGive(job->done);
    vTaskDelete(nullptr);
}

bool share(uint32_t count) {
    ShareJob job;
    job.count = count;
    job.ok = 0;
    job.err = 0;
    job.done = xSemaphoreCreateBinary();
    xTaskCreate(share_task, "share", 4096, &job, 5, nullptr);
    esp_err_t err = control(HOSTED_TEST_SET_MODE, HOSTED_TEST_SINK, 0);
    auto* h = reinterpret_cast<hosted_test_hdr_t*>(s_pkt);
    memset(h, 0, sizeof(*h));
    h->cmd = HOSTED_TEST_DATA;
    xSemaphoreTake(s_event, 0);
    int64_t t0 = esp_timer_get_time();
    for (uint32_t i = 0; i < 2000 && err == ESP_OK; i++) {
        h->seq = i;
        err = hosted_host_test_send(s_pkt, HOSTED_MAX_PAYLOAD, kWaitMs);
    }
    if (err == ESP_OK) err = control(HOSTED_TEST_STATS);
    if (err == ESP_OK && xSemaphoreTake(s_event, pdMS_TO_TICKS(kWaitMs)) != pdTRUE) err = ESP_ERR_TIMEOUT;
    int64_t us = esp_timer_get_time() - t0;
    xSemaphoreTake(job.done, portMAX_DELAY);
    vSemaphoreDelete(job.done);
    if (err != ESP_OK) return reply_err("share", err);
    harness_reply("OK sdio share %u %u %u %u", (unsigned)job.ok.load(), (unsigned)job.err.load(),
                  (unsigned)s_stats_bytes.load(), (unsigned)us);
    return true;
}

bool cmd_sdio(int argc, const char* const* argv, void*) {
    if (argc < 2) return false;
    const char* sub = argv[1];
    if (!strcmp(sub, "caps")) {
        harness_reply("OK sdio %x", (unsigned)hosted_host_ext_caps());
    } else if (!strcmp(sub, "log") && argc == 3) {
        esp_err_t err = hosted_host_log_forward(!strcmp(argv[2], "on"));
        if (err != ESP_OK) return reply_err("log", err);
    } else if (!strcmp(sub, "sink") && argc >= 4) {
        return sink(clamp_size(atoi(argv[2])), atoi(argv[3]), argc > 4 ? atoi(argv[4]) : 0);
    } else if (!strcmp(sub, "source") && argc == 4) {
        return source(clamp_size(atoi(argv[2])), atoi(argv[3]));
    } else if (!strcmp(sub, "share") && argc == 3) {
        return share(atoi(argv[2]));
    } else if (!strcmp(sub, "echo") && argc >= 4) {
        return echo(clamp_size(atoi(argv[2])), atoi(argv[3]), argc > 4 ? atoi(argv[4]) : 0);
    } else {
        return false;
    }
    return true;
}

}  // namespace

void sdio_bench_start() {
    s_event = xSemaphoreCreateBinary();
    hosted_host_set_test_rx(on_test, nullptr);
    harness_register("sdio", cmd_sdio, nullptr);
}

#else

void sdio_bench_start() {}

#endif
