/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Weak defaults for the board bus getters.
 */

#include "bsp.h"

__attribute__((weak)) i2c_master_bus_handle_t bsp_bus_get_i2c_handle(int i2c_port) {
    (void)i2c_port;
    return NULL;
}
