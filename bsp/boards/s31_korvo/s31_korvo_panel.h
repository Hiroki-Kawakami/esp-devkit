/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S31-Korvo panel bring-up split, mirroring the tab5 pattern: the board
 * entry (s31_korvo.c) sets up buses and hands off to this file for display +
 * touch. Only the RGB LCD is created here for now; GT1151 comes later.
 */

#pragma once
#include "bsp.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t s31_korvo_panel_init(const bsp_config_t *config);

#ifdef __cplusplus
}
#endif
