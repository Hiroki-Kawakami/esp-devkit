/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Vendor-neutral I2C transaction helpers for sensor drivers: one timeout
 * policy, plus the write -> exec-delay -> read pattern command-style chips
 * need (a STOP between the phases — chips that want repeated-start use
 * i2c_master_transmit_receive directly).
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include "esp_err.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define SENSOR_I2C_TIMEOUT_MS  100

esp_err_t sensor_i2c_write(i2c_master_dev_handle_t dev, const uint8_t *data, size_t len);
esp_err_t sensor_i2c_read(i2c_master_dev_handle_t dev, uint8_t *data, size_t len);
esp_err_t sensor_i2c_write_delay_read(i2c_master_dev_handle_t dev,
                                      const uint8_t *w, size_t wlen, uint32_t delay_ms,
                                      uint8_t *r, size_t rlen);

#ifdef __cplusplus
}
#endif
