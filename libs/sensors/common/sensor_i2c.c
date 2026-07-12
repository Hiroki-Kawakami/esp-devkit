/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sensor_i2c.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

esp_err_t sensor_i2c_write(i2c_master_dev_handle_t dev, const uint8_t *data, size_t len) {
    return i2c_master_transmit(dev, data, len, SENSOR_I2C_TIMEOUT_MS);
}

esp_err_t sensor_i2c_read(i2c_master_dev_handle_t dev, uint8_t *data, size_t len) {
    return i2c_master_receive(dev, data, len, SENSOR_I2C_TIMEOUT_MS);
}

esp_err_t sensor_i2c_write_delay_read(i2c_master_dev_handle_t dev,
                                      const uint8_t *w, size_t wlen, uint32_t delay_ms,
                                      uint8_t *r, size_t rlen) {
    esp_err_t err = sensor_i2c_write(dev, w, wlen);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(delay_ms));
    return sensor_i2c_read(dev, r, rlen);
}
