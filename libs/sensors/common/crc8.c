/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "crc8.h"

uint8_t sensor_crc8(uint8_t poly, uint8_t init, const uint8_t *data, size_t len) {
    uint8_t crc = init;
    for (size_t i = 0; i < len; i++) {
        crc ^= data[i];
        for (int j = 0; j < 8; j++) {
            crc = (crc & 0x80) ? (uint8_t)((crc << 1) ^ poly) : (uint8_t)(crc << 1);
        }
    }
    return crc;
}
