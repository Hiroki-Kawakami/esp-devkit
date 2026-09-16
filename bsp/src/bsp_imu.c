/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bsp.h"
#include "bsp_imu.h"
#include "bsp_dispatch.h"
#include "bsp_harness.h"
#include "sdkconfig.h"
#include "esp_timer.h"

#include <math.h>

#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#define ORIENTATION_INTERVAL_MS       100
#define ORIENTATION_STABLE_US         (300 * 1000)
#define ORIENTATION_FACE_ENTER_DEG    35.0f
#define ORIENTATION_FACE_EXIT_DEG     45.0f
#define ORIENTATION_ROTATION_HYST_DEG 15.0f
#define ORIENTATION_MIN_G             0.5f
#define ORIENTATION_MAX_G             1.5f
#define RAD_TO_DEG                    (180.0f / 3.14159265f)

static bsp_imu_t        *s_imu;
static SemaphoreHandle_t s_lock;
static bsp_imu_sample_t  s_last;
static bool              s_has_last;

static bsp_imu_orientation_cb_t        s_orientation_cb;
static void                           *s_orientation_arg;
static volatile bool                   s_orientation_enabled;
static volatile bsp_imu_orientation_t  s_orientation = BSP_IMU_ORIENTATION_UNKNOWN;
static bsp_imu_orientation_t           s_candidate   = BSP_IMU_ORIENTATION_UNKNOWN;
static int64_t                         s_candidate_since_us;

#if CONFIG_BSP_HARNESS
static bool             s_inject_active;
static bsp_imu_sample_t s_inject;
#endif

static uint32_t orientation_tick(void *ctx);
static bsp_dispatch_source_t s_orientation_source = { .tick = orientation_tick };

static inline void lock(void)   { xSemaphoreTake(s_lock, portMAX_DELAY); }
static inline void unlock(void) { xSemaphoreGive(s_lock); }

static float axis_value(const bsp_vec3_t *v, int8_t axis) {
    float value;
    switch (axis < 0 ? -axis : axis) {
        case 1:  value = v->x; break;
        case 2:  value = v->y; break;
        case 3:  value = v->z; break;
        default: return 0.0f;
    }
    return axis < 0 ? -value : value;
}

static bsp_vec3_t remap(const bsp_vec3_t *v, bsp_imu_axis_map_t map) {
    return (bsp_vec3_t){ axis_value(v, map.x), axis_value(v, map.y), axis_value(v, map.z) };
}

void bsp_imu_set_active(bsp_imu_t *imu) {
    if (!s_lock) s_lock = xSemaphoreCreateMutex();
    s_imu = imu;
    s_has_last = false;
}

uint32_t bsp_imu_get_caps(void) {
    return s_imu ? s_imu->caps : 0;
}

esp_err_t bsp_imu_configure(const bsp_imu_config_t *config, bsp_imu_config_t *out_actual) {
    if (!s_imu || !s_imu->configure) return ESP_ERR_NOT_SUPPORTED;
    lock();
    bsp_imu_config_t request = s_imu->config;
    if (config->odr_hz)         request.odr_hz         = config->odr_hz;
    if (config->accel_range_g)  request.accel_range_g  = config->accel_range_g;
    if (config->gyro_range_dps) request.gyro_range_dps = config->gyro_range_dps;
    bsp_imu_config_t actual = request;
    esp_err_t err = s_imu->configure(s_imu, &request, &actual);
    if (err == ESP_OK) s_imu->config = actual;
    unlock();
    if (err == ESP_OK && out_actual) *out_actual = actual;
    return err;
}

esp_err_t bsp_imu_get_config(bsp_imu_config_t *out) {
    if (!s_imu) return ESP_ERR_NOT_SUPPORTED;
    lock();
    *out = s_imu->config;
    unlock();
    return ESP_OK;
}

static esp_err_t refresh_locked(void) {
#if CONFIG_BSP_HARNESS
    if (s_inject_active) {
        s_last = s_inject;
        s_last.timestamp_us = esp_timer_get_time();
        s_has_last = true;
        return ESP_OK;
    }
#endif
    bsp_vec3_t accel = {0}, gyro = {0};
    bool updated = false;
    esp_err_t err = s_imu->read(s_imu, &accel, &gyro, &updated);
    if (err != ESP_OK) return err;
    if (updated || !s_has_last) {
        s_last.accel        = remap(&accel, s_imu->axis_map);
        s_last.gyro         = remap(&gyro, s_imu->axis_map);
        s_last.timestamp_us = esp_timer_get_time();
        s_has_last = true;
    }
    return ESP_OK;
}

esp_err_t bsp_imu_read(bsp_imu_sample_t *out) {
    if (!s_imu) return ESP_ERR_NOT_SUPPORTED;
    lock();
    esp_err_t err = refresh_locked();
    if (err == ESP_OK) *out = s_last;
    unlock();
    return err;
}

static float angle_distance_deg(float a, float b) {
    float d = fmodf(fabsf(a - b), 360.0f);
    return d > 180.0f ? 360.0f - d : d;
}

static bool is_rotation(bsp_imu_orientation_t o) {
    return o >= BSP_IMU_ORIENTATION_ROTATION_0 && o <= BSP_IMU_ORIENTATION_ROTATION_270;
}

static bsp_imu_orientation_t classify(const bsp_vec3_t *a, bsp_imu_orientation_t current) {
    float norm = sqrtf(a->x * a->x + a->y * a->y + a->z * a->z);
    if (norm < ORIENTATION_MIN_G || norm > ORIENTATION_MAX_G) return BSP_IMU_ORIENTATION_UNKNOWN;

    bool face = current == BSP_IMU_ORIENTATION_FACE_UP || current == BSP_IMU_ORIENTATION_FACE_DOWN;
    float tilt_deg = acosf(fminf(1.0f, fabsf(a->z) / norm)) * RAD_TO_DEG;
    if (tilt_deg < (face ? ORIENTATION_FACE_EXIT_DEG : ORIENTATION_FACE_ENTER_DEG)) {
        return a->z < 0.0f ? BSP_IMU_ORIENTATION_FACE_UP : BSP_IMU_ORIENTATION_FACE_DOWN;
    }

    float up_deg = atan2f(-a->x, -a->y) * RAD_TO_DEG;
    if (up_deg < 0.0f) up_deg += 360.0f;
    if (is_rotation(current) &&
        angle_distance_deg(up_deg, (float)current * 90.0f) <= 45.0f + ORIENTATION_ROTATION_HYST_DEG) {
        return current;
    }
    return (bsp_imu_orientation_t)((int)lroundf(up_deg / 90.0f) % 4);
}

static uint32_t orientation_tick(void *ctx) {
    (void)ctx;
    if (!s_imu) return BSP_DISPATCH_IDLE;

    bool fire = false;
    bsp_imu_orientation_t value = BSP_IMU_ORIENTATION_UNKNOWN;
    lock();
    bool enabled = s_orientation_enabled;
    if (enabled) {
        esp_err_t err = refresh_locked();
        bsp_imu_orientation_t next = err == ESP_OK ? classify(&s_last.accel, s_orientation)
                                                   : BSP_IMU_ORIENTATION_UNKNOWN;
        int64_t now = esp_timer_get_time();
        if (next != s_candidate) {
            s_candidate = next;
            s_candidate_since_us = now;
        }
        if (s_candidate != BSP_IMU_ORIENTATION_UNKNOWN && s_candidate != s_orientation &&
            now - s_candidate_since_us >= ORIENTATION_STABLE_US) {
            s_orientation = s_candidate;
            value = s_candidate;
            fire = true;
        }
    }
    unlock();

    if (fire) {
        bsp_imu_orientation_cb_t cb = s_orientation_cb;
        if (cb) cb(value, s_orientation_arg);
    }
    return enabled ? ORIENTATION_INTERVAL_MS : BSP_DISPATCH_IDLE;
}

void bsp_imu_set_orientation_cb(bsp_imu_orientation_cb_t cb, void *arg) {
    s_orientation_arg = arg;
    s_orientation_cb  = cb;
}

esp_err_t bsp_imu_set_orientation_enabled(bool enabled) {
    if (!s_imu || !(s_imu->caps & BSP_IMU_CAP_ACCEL)) return ESP_ERR_NOT_SUPPORTED;
    lock();
    bool changed = s_orientation_enabled != enabled;
    if (changed) {
        s_orientation_enabled = enabled;
        s_orientation = BSP_IMU_ORIENTATION_UNKNOWN;
        s_candidate   = BSP_IMU_ORIENTATION_UNKNOWN;
    }
    unlock();
    if (!changed || !enabled) return ESP_OK;

    esp_err_t err = bsp_dispatch_add_source(&s_orientation_source);
    if (err != ESP_OK) {
        lock();
        s_orientation_enabled = false;
        unlock();
        return err;
    }
    bsp_dispatch_notify(&s_orientation_source);
    return ESP_OK;
}

bool bsp_imu_get_orientation_enabled(void) {
    return s_orientation_enabled;
}

bsp_imu_orientation_t bsp_imu_get_orientation(void) {
    return s_orientation;
}

#if CONFIG_BSP_HARNESS
bool bsp_harness_imu_present(void) {
    return s_imu != NULL;
}

esp_err_t bsp_harness_imu_inject(const bsp_imu_sample_t *sample) {
    if (!s_imu) return ESP_ERR_NOT_SUPPORTED;
    lock();
    s_inject_active = sample != NULL;
    if (sample) s_inject = *sample;
    else        s_has_last = false;
    unlock();
    return ESP_OK;
}
#endif
