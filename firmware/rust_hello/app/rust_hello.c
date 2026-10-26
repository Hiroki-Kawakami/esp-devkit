/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "rust_hello.h"
#include "rust_hello_weight.h"

uint32_t rust_hello_checksum(const uint8_t *data, size_t len) {
    uint32_t sum = 0;
    for (size_t i = 0; i < len; i++) {
        sum = sum * 31 + rust_hello_weight(data[i]);
    }
    return sum;
}
