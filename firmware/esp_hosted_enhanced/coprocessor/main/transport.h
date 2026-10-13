/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "hosted_wire.h"

typedef struct {
    void (*on_sta)(uint8_t *payload, uint16_t len);
    void (*on_serial)(const uint8_t *payload, uint16_t len, uint8_t flags);
    void (*on_test)(const uint8_t *payload, uint16_t len);
    void (*on_hci)(uint8_t type, const uint8_t *payload, uint16_t len);
    void (*on_log_enable)(bool enable);
    void (*on_open)(void);
    uint8_t caps;
    uint32_t ext_caps;
} transport_cbs_t;

esp_err_t transport_init(const transport_cbs_t *cbs);

bool transport_is_open(void);

/* Copies `data`; fails with ESP_ERR_TIMEOUT when no buffer frees up within `wait`. */
esp_err_t transport_send(hosted_if_t if_type, uint8_t flags, uint16_t seq,
                         const void *data, uint16_t len, TickType_t wait);

/* Fill a buffer in place: get returns HOSTED_MAX_PAYLOAD bytes of payload
 * space (NULL on timeout), and every buffer got goes back through put. */
uint8_t *transport_tx_get(hosted_if_t if_type, TickType_t wait);
esp_err_t transport_tx_put(uint8_t *payload, hosted_if_t if_type, uint8_t flags, uint16_t seq,
                           uint16_t len);
