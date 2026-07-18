/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bsp.h"
#include "bsp_sd.h"

static bsp_sd_t *s_sd;

void bsp_sd_set_active(bsp_sd_t *sd) {
    s_sd = sd;
}

esp_err_t bsp_sd_mount(const char *mount_point, const bsp_sd_mount_config_t *config) {
    if (!mount_point) return ESP_ERR_INVALID_ARG;
    if (!s_sd) return ESP_ERR_NOT_SUPPORTED;
    return s_sd->mount(s_sd, mount_point, config);
}

esp_err_t bsp_sd_unmount(void) {
    if (!s_sd) return ESP_ERR_NOT_SUPPORTED;
    return s_sd->unmount(s_sd);
}

bool bsp_sd_is_mounted(void) {
    return s_sd && s_sd->is_mounted(s_sd);
}
