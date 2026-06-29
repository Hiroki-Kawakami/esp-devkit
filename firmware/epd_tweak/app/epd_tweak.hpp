/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "bsp.h"

void app_entry();

// Standing mode for end-of-flush refresh; NONE = stage GRAM only.
void epd_set_default_refresh_mode(bsp_epd_mode_t mode);
// One-shot override consumed by the next flush.
void epd_set_next_refresh_mode(bsp_epd_mode_t mode);
