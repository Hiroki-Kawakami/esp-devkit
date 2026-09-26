/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * RPC framing over the hosted serial interface: endpoint/data TLV, fragmented
 * to fit the transport packets.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

/* Receives ownership of a heap-allocated protobuf message. */
typedef void (*serial_msg_cb_t)(uint8_t *msg, size_t len);

void serial_init(serial_msg_cb_t on_msg);

/* Transport rx hook, called from a single task. */
void serial_on_packet(const uint8_t *payload, uint16_t len, uint8_t flags);

esp_err_t serial_send(bool event, const uint8_t *msg, size_t len);
