/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>

/* Turning a clock on also resets the block. */
void media_engine_jpeg_clock(bool on);
void media_engine_ppa_clock(bool on);
