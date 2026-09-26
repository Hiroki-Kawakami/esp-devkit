/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Test channel peer: sinks, echoes or generates packets so the SDIO link can
 * be measured without Wi-Fi.
 */

#pragma once

#include <stdint.h>

void bench_init(void);

/* Transport rx hook; a configured per-packet delay stalls the rx task on
 * purpose to hold receive buffers like a slow consumer. */
void bench_on_packet(const uint8_t *payload, uint16_t len);
