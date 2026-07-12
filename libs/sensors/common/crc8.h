/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Parameterized CRC-8, MSB-first, no reflection / final XOR (add those
 * parameters when a chip needs them).
 */

#pragma once
#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

uint8_t sensor_crc8(uint8_t poly, uint8_t init, const uint8_t *data, size_t len);

#ifdef __cplusplus
}
#endif
