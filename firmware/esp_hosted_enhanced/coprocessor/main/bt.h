/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "rpc.h"

/* Host -> controller, from the transport rx task. */
void bt_on_host_packet(uint8_t type, const uint8_t *payload, uint16_t len);

void bt_rpc_feature_control(const Rpc *req, Rpc *resp);
