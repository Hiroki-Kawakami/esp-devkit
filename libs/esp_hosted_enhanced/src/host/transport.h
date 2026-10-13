/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "hosted_wire.h"

/* Resets the coprocessor and blocks until its INIT event arrived. */
esp_err_t hosted_transport_start(void);

bool hosted_transport_ready(void);

/* HOSTED_CAP_* from the last INIT event. */
uint8_t hosted_transport_caps(void);

/* HOSTED_EXT_CAP_* from the last INIT event; 0 for a stock coprocessor. */
uint32_t hosted_transport_ext_caps(void);

typedef void (*hosted_transport_rx_cb_t)(const uint8_t *data, size_t len, void *arg);
void hosted_transport_set_test_rx(hosted_transport_rx_cb_t cb, void *arg);

/* Copies `data`; ESP_ERR_TIMEOUT when no buffer frees up within `wait`. */
esp_err_t hosted_transport_send(hosted_if_t if_type, uint8_t flags,
                                const void *data, size_t len, TickType_t wait);

/* Fill a buffer in place: get returns HOSTED_MAX_PAYLOAD bytes of payload
 * space (NULL on timeout or before the link is up), and every buffer got goes
 * back through put. */
uint8_t *hosted_transport_tx_get(hosted_if_t if_type, TickType_t wait);
void hosted_transport_tx_put(uint8_t *payload, hosted_if_t if_type, uint8_t flags,
                             uint8_t pkt_type, size_t len);

/* Receivers, called from the transport rx task. */
void hosted_rpc_on_serial(const uint8_t *payload, uint16_t len, uint8_t flags);
void hosted_wifi_on_sta(const uint8_t *frame, uint16_t len);
void hosted_bt_on_hci(const uint8_t *payload, uint16_t len);
