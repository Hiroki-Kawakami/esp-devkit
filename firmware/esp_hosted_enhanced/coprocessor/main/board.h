/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "driver/sdio_slave.h"

void board_init(void);
sdio_slave_timing_t board_sdio_timing(void);
uint32_t board_sdio_flags(void);
/* Called once sdio_slave_initialize() has claimed the pins. */
void board_sdio_pins_ready(void);
