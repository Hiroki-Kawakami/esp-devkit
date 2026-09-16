/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imu_sim.h"
#include "sdl_panel.h"

static esp_err_t sim_read(bsp_imu_t *self, bsp_vec3_t *accel, bsp_vec3_t *gyro, bool *out_updated) {
    (void)self;
    switch (sdl_panel_get_view_rotation()) {
        case BSP_ROTATION_90:  *accel = (bsp_vec3_t){ -1.0f,  0.0f, 0.0f }; break;
        case BSP_ROTATION_180: *accel = (bsp_vec3_t){  0.0f,  1.0f, 0.0f }; break;
        case BSP_ROTATION_270: *accel = (bsp_vec3_t){  1.0f,  0.0f, 0.0f }; break;
        default:               *accel = (bsp_vec3_t){  0.0f, -1.0f, 0.0f }; break;
    }
    *gyro = (bsp_vec3_t){ 0 };
    *out_updated = true;
    return ESP_OK;
}

static esp_err_t sim_configure(bsp_imu_t *self, const bsp_imu_config_t *config, bsp_imu_config_t *out_actual) {
    (void)self;
    *out_actual = *config;
    out_actual->gyro_range_dps = 0;
    return ESP_OK;
}

static esp_err_t sim_deinit(bsp_imu_t *self) {
    (void)self;
    return ESP_OK;
}

static bsp_imu_t s_imu = {
    .read      = sim_read,
    .configure = sim_configure,
    .deinit    = sim_deinit,
    .caps      = BSP_IMU_CAP_ACCEL,
    .axis_map  = { .x = 1, .y = 2, .z = 3 },
    .config    = { .odr_hz = 100, .accel_range_g = 8 },
};

esp_err_t imu_sim_create(bsp_imu_t **out_imu) {
    if (!out_imu) return ESP_ERR_INVALID_ARG;
    *out_imu = &s_imu;
    return ESP_OK;
}
