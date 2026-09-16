/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "bsp_types.h"

typedef struct { int8_t x, y, z; } bsp_imu_axis_map_t;

typedef struct bsp_imu bsp_imu_t;

struct bsp_imu {
    esp_err_t (*read)(bsp_imu_t *self, bsp_vec3_t *accel, bsp_vec3_t *gyro, bool *out_updated);
    esp_err_t (*configure)(bsp_imu_t *self, const bsp_imu_config_t *config, bsp_imu_config_t *out_actual);
    esp_err_t (*deinit)(bsp_imu_t *self);

    uint32_t           caps;
    bsp_imu_axis_map_t axis_map;
    bsp_imu_config_t   config;
};

void bsp_imu_set_active(bsp_imu_t *imu);
