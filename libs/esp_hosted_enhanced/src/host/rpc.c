/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "host/rpc.h"

#include <stdlib.h>
#include <string.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "host/transport.h"

#define TLV_HEADER_LEN (3 + HOSTED_SERIAL_EP_LEN + 3)

static const char *TAG = "hosted_rpc";

static SemaphoreHandle_t s_call_lock;
static SemaphoreHandle_t s_done;
static uint32_t s_uid;
static volatile uint32_t s_pending_uid;
static Rpc *s_response;
static uint8_t *s_rx_buf;
static size_t s_rx_len;
static bool s_rx_overflow;

void hosted_rpc_init(void) {
    if (s_call_lock) return;
    s_call_lock = xSemaphoreCreateMutex();
    s_done = xSemaphoreCreateBinary();
    s_rx_buf = malloc(HOSTED_MAX_SERIAL);
    assert(s_call_lock && s_done && s_rx_buf);
}

static esp_err_t send_serial(const uint8_t *msg, size_t len) {
    size_t total = TLV_HEADER_LEN + len;
    if (total > HOSTED_MAX_SERIAL) return ESP_ERR_INVALID_SIZE;
    uint8_t *buf = malloc(total);
    if (!buf) return ESP_ERR_NO_MEM;
    buf[0] = HOSTED_SERIAL_TLV_EP;
    buf[1] = HOSTED_SERIAL_EP_LEN;
    buf[2] = 0;
    memcpy(&buf[3], HOSTED_SERIAL_EP_RSP, HOSTED_SERIAL_EP_LEN);
    size_t pos = 3 + HOSTED_SERIAL_EP_LEN;
    buf[pos++] = HOSTED_SERIAL_TLV_DATA;
    buf[pos++] = len & 0xff;
    buf[pos++] = len >> 8;
    memcpy(&buf[pos], msg, len);

    esp_err_t err = ESP_OK;
    for (size_t off = 0; off < total && err == ESP_OK; off += HOSTED_MAX_PAYLOAD) {
        size_t n = total - off > HOSTED_MAX_PAYLOAD ? HOSTED_MAX_PAYLOAD : total - off;
        uint8_t flags = off + n < total ? HOSTED_FLAG_MORE_FRAGMENT : 0;
        err = hosted_transport_send(HOSTED_IF_SERIAL, flags, buf + off, n, portMAX_DELAY);
    }
    free(buf);
    return err;
}

Rpc *hosted_rpc_call(Rpc *req, uint32_t timeout_ms, esp_err_t *err) {
    xSemaphoreTake(s_call_lock, portMAX_DELAY);
    if (++s_uid == 0) s_uid = 1;
    req->msg_type = RPC_TYPE__Req;
    req->uid = s_uid;

    size_t n = rpc__get_packed_size(req);
    uint8_t *buf = malloc(n);
    Rpc *resp = NULL;
    *err = ESP_ERR_NO_MEM;
    if (buf) {
        rpc__pack(req, buf);
        xSemaphoreTake(s_done, 0);
        if (s_response) {
            rpc__free_unpacked(s_response, NULL);
            s_response = NULL;
        }
        s_pending_uid = s_uid;
        *err = send_serial(buf, n);
        free(buf);
        if (*err == ESP_OK) {
            if (xSemaphoreTake(s_done, pdMS_TO_TICKS(timeout_ms)) == pdTRUE) {
                resp = s_response;
                s_response = NULL;
                if (resp->msg_id == RPC_ID__Resp_Base) {
                    rpc__free_unpacked(resp, NULL);
                    resp = NULL;
                    *err = ESP_ERR_NOT_SUPPORTED;
                }
            } else {
                *err = ESP_ERR_TIMEOUT;
                ESP_LOGE(TAG, "no response to request %d", req->msg_id);
            }
        }
        s_pending_uid = 0;
    }
    xSemaphoreGive(s_call_lock);
    return resp;
}

static void on_message(const uint8_t *msg, size_t len) {
    Rpc *rpc = rpc__unpack(NULL, len, msg);
    if (!rpc) {
        ESP_LOGW(TAG, "undecodable message dropped");
        return;
    }
    if (rpc->msg_type == RPC_TYPE__Event) {
        hosted_wifi_on_event(rpc);
    } else if (rpc->msg_type == RPC_TYPE__Resp && rpc->uid && rpc->uid == s_pending_uid) {
        s_pending_uid = 0;
        s_response = rpc;
        xSemaphoreGive(s_done);
        return;
    } else {
        ESP_LOGW(TAG, "stale response %d dropped", rpc->msg_id);
    }
    rpc__free_unpacked(rpc, NULL);
}

static void deliver(void) {
    const uint8_t *p = s_rx_buf;
    if (s_rx_len < TLV_HEADER_LEN || p[0] != HOSTED_SERIAL_TLV_EP) return;
    size_t pos = 3 + (p[1] | p[2] << 8);
    if (pos + 3 > s_rx_len || p[pos] != HOSTED_SERIAL_TLV_DATA) return;
    size_t len = p[pos + 1] | p[pos + 2] << 8;
    pos += 3;
    if (len && pos + len <= s_rx_len) on_message(p + pos, len);
}

void hosted_rpc_on_serial(const uint8_t *payload, uint16_t len, uint8_t flags) {
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
