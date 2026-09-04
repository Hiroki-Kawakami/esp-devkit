/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 M-Bus display-module bring-up: split from tab5.c so the device
 * entry keeps the port dispatch while this owns the module's pin map.
 */

#pragma once
#include "bsp.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t tab5_module_display_attach(const bsp_display_module_config_t *config,
                                     bsp_panel_id_t *out_panel);

#ifdef __cplusplus
}
#endif
