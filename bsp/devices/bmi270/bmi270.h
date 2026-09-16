/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "bsp_types.h"
#include "bsp_imu.h"
#include "driver/i2c_master.h"

#define BMI270_I2C_ADDR        0x68
#define BMI270_I2C_DEFAULT_HZ  (400 * 1000)

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t                 i2c_address;
    uint32_t                clock_hz;
    bsp_imu_axis_map_t      axis_map;
    bsp_imu_config_t        config;
} bmi270_config_t;

esp_err_t bmi270_imu_create(const bmi270_config_t *config, bsp_imu_t **out_imu);
