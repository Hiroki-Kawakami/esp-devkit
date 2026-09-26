/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bench.h"

#include <string.h>

#include "esp_rom_sys.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "transport.h"

static TaskHandle_t s_source_task;
static volatile hosted_test_mode_t s_mode;
static volatile uint32_t s_delay_us;
static uint32_t s_packets;
static uint32_t s_bytes;
static uint32_t s_source_size;
static uint32_t s_source_count;

static void reply(hosted_test_cmd_t cmd, uint32_t arg0, uint32_t arg1) {
    hosted_test_hdr_t h = {.cmd = cmd, .arg = {arg0, arg1}};
    transport_send(HOSTED_IF_TEST, 0, 0, &h, sizeof(h), portMAX_DELAY);
}

static void source_task(void *arg) {
    static uint8_t pkt[HOSTED_MAX_PAYLOAD];
    for (;;) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
        uint32_t size = s_source_size;
        if (size < sizeof(hosted_test_hdr_t)) size = sizeof(hosted_test_hdr_t);
        if (size > sizeof(pkt)) size = sizeof(pkt);
        hosted_test_hdr_t *h = (hosted_test_hdr_t *)pkt;
        memset(h, 0, sizeof(*h));
        h->cmd = HOSTED_TEST_DATA;
        for (uint32_t i = 0; i < s_source_count; i++) {
            h->seq = i;
            transport_send(HOSTED_IF_TEST, 0, 0, pkt, size, portMAX_DELAY);
        }
    }
}

void bench_on_packet(const uint8_t *payload, uint16_t len) {
    if (len < sizeof(hosted_test_hdr_t)) return;
    hosted_test_hdr_t h;
    memcpy(&h, payload, sizeof(h));
    switch (h.cmd) {
    case HOSTED_TEST_DATA:
        s_packets++;
        s_bytes += len;
        if (s_delay_us) esp_rom_delay_us(s_delay_us);
        if (s_mode == HOSTED_TEST_ECHO) transport_send(HOSTED_IF_TEST, 0, 0, payload, len, portMAX_DELAY);
        break;
    case HOSTED_TEST_SET_MODE:
        s_mode = h.arg[0];
        s_delay_us = h.arg[1];
        s_packets = 0;
        s_bytes = 0;
        break;
    case HOSTED_TEST_STATS:
        reply(HOSTED_TEST_STATS, s_packets, s_bytes);
        break;
    case HOSTED_TEST_SOURCE:
        s_source_size = h.arg[0];
        s_source_count = h.arg[1];
        xTaskNotifyGive(s_source_task);
        break;
    default:
        break;
    }
}

void bench_init(void) {
    xTaskCreate(source_task, "bench_src", 2560, NULL, 20, &s_source_task);
}
