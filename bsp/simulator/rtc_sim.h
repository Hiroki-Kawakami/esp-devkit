/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_rtc provider backed by the host clock. The board's simulator
 * bring-up registers it with bsp_rtc_set_active().
 */

#pragma once
#include "bsp_rtc.h"

esp_err_t rtc_sim_create(bsp_rtc_t **out_rtc);
