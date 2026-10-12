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

#include "hosted_wire.h"

#define SERIAL_HEADROOM (3 + HOSTED_SERIAL_EP_LEN + 3)

/* Receives ownership of the heap block `owner`, which holds the protobuf
 * message at msg. */
typedef void (*serial_msg_cb_t)(uint8_t *owner, const uint8_t *msg, size_t len);

void serial_init(serial_msg_cb_t on_msg);

/* Transport rx hook, called from a single task. */
void serial_on_packet(const uint8_t *payload, uint16_t len, uint8_t flags);

/* buf: SERIAL_HEADROOM bytes for the framing, then the len message bytes. */
esp_err_t serial_send(bool event, uint8_t *buf, size_t len);
