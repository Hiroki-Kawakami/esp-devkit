/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "bsp.h"

typedef struct bsp_sd bsp_sd_t;

struct bsp_sd {
    esp_err_t (*mount)(bsp_sd_t *self, const char *mount_point,
                       const bsp_sd_mount_config_t *config);
    esp_err_t (*unmount)(bsp_sd_t *self);
    bool (*is_mounted)(bsp_sd_t *self);
    esp_err_t (*deinit)(bsp_sd_t *self);
};

void bsp_sd_set_active(bsp_sd_t *sd);
