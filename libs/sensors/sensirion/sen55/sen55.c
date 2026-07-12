/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sen55.h"
#include "sensirion_i2c.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define SEN55_CMD_START            0x0021
#define SEN55_CMD_START_RHT_GAS    0x0037
#define SEN55_CMD_STOP             0x0104
#define SEN55_CMD_DATA_READY       0x0202
#define SEN55_CMD_READ_VALUES      0x03C4
#define SEN55_CMD_FAN_CLEAN        0x5607
#define SEN55_CMD_READ_SERIAL      0xD033
#define SEN55_CMD_READ_STATUS      0xD206
#define SEN55_CMD_RESET            0xD304

#define SEN55_UINT_INVALID  0xFFFF
#define SEN55_INT_INVALID   0x7FFF

struct sen55 {
    i2c_master_dev_handle_t i2c_dev;
};

esp_err_t sen55_create(const sen55_config_t *config, sen55_t **out) {
    if (!config || !config->i2c_bus || !out) return ESP_ERR_INVALID_ARG;
    uint8_t address = config->i2c_address ? config->i2c_address : SEN55_I2C_ADDR;
    esp_err_t err = i2c_master_probe(config->i2c_bus, address, SENSOR_I2C_TIMEOUT_MS);
    if (err != ESP_OK) return err;

    sen55_t *sensor = calloc(1, sizeof(*sensor));
    if (!sensor) return ESP_ERR_NO_MEM;
    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = address,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : SEN55_I2C_DEFAULT_HZ,
    };
    err = i2c_master_bus_add_device(config->i2c_bus, &dev_cfg, &sensor->i2c_dev);
    if (err != ESP_OK) {
        free(sensor);
        return err;
    }
    *out = sensor;
    return ESP_OK;
}

void sen55_delete(sen55_t *sensor) {
    if (!sensor) return;
    i2c_master_bus_rm_device(sensor->i2c_dev);
    free(sensor);
}

static esp_err_t write_cmd_delay(sen55_t *sensor, uint16_t cmd, uint32_t delay_ms) {
    esp_err_t err = sensirion_write_cmd(sensor->i2c_dev, cmd);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(delay_ms));
    return ESP_OK;
}

esp_err_t sen55_start(sen55_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SEN55_CMD_START, 50);
}

esp_err_t sen55_start_rht_gas_only(sen55_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SEN55_CMD_START_RHT_GAS, 50);
}

esp_err_t sen55_stop(sen55_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SEN55_CMD_STOP, 200);
}

esp_err_t sen55_data_ready(sen55_t *sensor, bool *out_ready) {
    if (!sensor || !out_ready) return ESP_ERR_INVALID_ARG;
    uint16_t word;
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SEN55_CMD_DATA_READY, 20, &word, 1);
    if (err != ESP_OK) return err;
    *out_ready = (word & 0x00FF) != 0;
    return ESP_OK;
}

static float dec_uint(uint16_t raw, float div) {
    return raw == SEN55_UINT_INVALID ? NAN : raw / div;
}

static float dec_int(uint16_t raw, float div) {
    return (int16_t)raw == SEN55_INT_INVALID ? NAN : (int16_t)raw / div;
}

esp_err_t sen55_read(sen55_t *sensor, sen55_data_t *out) {
    if (!sensor || !out) return ESP_ERR_INVALID_ARG;
    uint16_t w[8];
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SEN55_CMD_READ_VALUES, 20, w, 8);
    if (err != ESP_OK) return err;
    out->pm1_0       = dec_uint(w[0], 10.0f);
    out->pm2_5       = dec_uint(w[1], 10.0f);
    out->pm4_0       = dec_uint(w[2], 10.0f);
    out->pm10_0      = dec_uint(w[3], 10.0f);
    out->humidity    = dec_int(w[4], 100.0f);
    out->temperature = dec_int(w[5], 200.0f);
    out->voc_index   = dec_int(w[6], 10.0f);
    out->nox_index   = dec_int(w[7], 10.0f);
    return ESP_OK;
}

esp_err_t sen55_fan_clean(sen55_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SEN55_CMD_FAN_CLEAN, 20);
}

esp_err_t sen55_read_status(sen55_t *sensor, uint32_t *out_flags) {
    if (!sensor || !out_flags) return ESP_ERR_INVALID_ARG;
    uint16_t w[2];
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SEN55_CMD_READ_STATUS, 20, w, 2);
    if (err != ESP_OK) return err;
    *out_flags = ((uint32_t)w[0] << 16) | w[1];
    return ESP_OK;
}

esp_err_t sen55_read_serial(sen55_t *sensor, char *out, size_t len) {
    if (!sensor || !out || len == 0) return ESP_ERR_INVALID_ARG;
    uint8_t raw[32];
    esp_err_t err = sensirion_write_cmd(sensor->i2c_dev, SEN55_CMD_READ_SERIAL);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(50));
    err = sensirion_read_bytes(sensor->i2c_dev, raw, sizeof(raw));
    if (err != ESP_OK) return err;
    size_t copy_len = len - 1 < sizeof(raw) ? len - 1 : sizeof(raw);
    memcpy(out, raw, copy_len);
    out[copy_len] = '\0';
    return ESP_OK;
}

esp_err_t sen55_reset(sen55_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SEN55_CMD_RESET, 200);
}
