/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host counterpart of ESP-IDF <esp_attr.h>. The host has no IRAM/DRAM split, so
 * the placement attributes expand to nothing.
 */

#pragma once

#define IRAM_ATTR
#define DRAM_ATTR
