/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * LilyGo T5 ePaper S3: minimum board-side bsp_init orchestration.
 */

#include "bsp.h"
#include "bsp_dispatch.h"
#include "lilygo_t5_panel.h"

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    bsp_config_t defaults = {0};
    if (!config) config = &defaults;
    return lilygo_t5_panel_init(config);
}
