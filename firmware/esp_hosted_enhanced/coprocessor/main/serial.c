/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "serial.h"

#include <stdlib.h>
#include <string.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "hosted_wire.h"
#include "transport.h"

#define TLV_HEADER_LEN  (3 + HOSTED_SERIAL_EP_LEN + 3)
#define TX_FRAGMENT     1500

static const char *TAG = "serial";

static serial_msg_cb_t s_on_msg;
static SemaphoreHandle_t s_tx_lock;
static uint16_t s_tx_seq;
static uint8_t *s_rx_buf;
static size_t s_rx_len;
static bool s_rx_overflow;

void serial_init(serial_msg_cb_t on_msg) {
    s_on_msg = on_msg;
    s_tx_lock = xSemaphoreCreateMutex();
    s_rx_buf = malloc(HOSTED_MAX_SERIAL);
    assert(s_tx_lock && s_rx_buf);
}

static void deliver(void) {
    const uint8_t *p = s_rx_buf;
    if (s_rx_len < TLV_HEADER_LEN || p[0] != HOSTED_SERIAL_TLV_EP) return;
    uint16_t ep_len = p[1] | (p[2] << 8);
    size_t pos = 3 + ep_len;
    if (pos + 3 > s_rx_len || p[pos] != HOSTED_SERIAL_TLV_DATA) return;
    uint16_t data_len = p[pos + 1] | (p[pos + 2] << 8);
    pos += 3;
    if (data_len == 0 || pos + data_len > s_rx_len) return;

    uint8_t *msg = malloc(data_len);
    if (!msg) return;
    memcpy(msg, p + pos, data_len);
    s_on_msg(msg, data_len);
}

void serial_on_packet(const uint8_t *payload, uint16_t len, uint8_t flags) {
    if (s_rx_len + len > HOSTED_MAX_SERIAL) {
        s_rx_overflow = true;
    } else if (!s_rx_overflow) {
        memcpy(s_rx_buf + s_rx_len, payload, len);
        s_rx_len += len;
    }
    if (flags & HOSTED_FLAG_MORE_FRAGMENT) return;

    if (s_rx_overflow) {
        ESP_LOGW(TAG, "rpc message over %d bytes dropped", HOSTED_MAX_SERIAL);
    } else {
        deliver();
    }
    s_rx_len = 0;
    s_rx_overflow = false;
}

esp_err_t serial_send(bool event, const uint8_t *msg, size_t len) {
    size_t total = TLV_HEADER_LEN + len;
    if (total > HOSTED_MAX_SERIAL) return ESP_ERR_INVALID_SIZE;
    uint8_t *buf = malloc(total);
    if (!buf) return ESP_ERR_NO_MEM;

    buf[0] = HOSTED_SERIAL_TLV_EP;
    buf[1] = HOSTED_SERIAL_EP_LEN;
    buf[2] = 0;
    memcpy(&buf[3], event ? HOSTED_SERIAL_EP_EVT : HOSTED_SERIAL_EP_RSP, HOSTED_SERIAL_EP_LEN);
    size_t pos = 3 + HOSTED_SERIAL_EP_LEN;
    buf[pos++] = HOSTED_SERIAL_TLV_DATA;
    buf[pos++] = len & 0xff;
    buf[pos++] = len >> 8;
    memcpy(&buf[pos], msg, len);

    esp_err_t err = ESP_OK;
    xSemaphoreTake(s_tx_lock, portMAX_DELAY);
    for (size_t off = 0; off < total && err == ESP_OK; off += TX_FRAGMENT) {
        size_t n = total - off > TX_FRAGMENT ? TX_FRAGMENT : total - off;
        uint8_t flags = off + n < total ? HOSTED_FLAG_MORE_FRAGMENT : 0;
        err = transport_send(HOSTED_IF_SERIAL, flags, s_tx_seq++, buf + off, n, portMAX_DELAY);
    }
    xSemaphoreGive(s_tx_lock);
    free(buf);
    return err;
}
