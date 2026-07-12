/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "scd40.h"
#include "sensirion_i2c.h"
#include <stdlib.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define SCD40_CMD_START             0x21B1
#define SCD40_CMD_START_LOW_POWER   0x21AC
#define SCD40_CMD_STOP              0x3F86
#define SCD40_CMD_DATA_READY        0xE4B8
#define SCD40_CMD_READ_MEASUREMENT  0xEC05
#define SCD40_CMD_SET_ASC           0x2416
#define SCD40_CMD_GET_ASC           0x2313
#define SCD40_CMD_FORCED_RECAL      0x362F
#define SCD40_CMD_SET_TEMP_OFFSET   0x241D
#define SCD40_CMD_SET_ALTITUDE      0x2427
#define SCD40_CMD_SET_PRESSURE      0xE000
#define SCD40_CMD_PERSIST_SETTINGS  0x3615
#define SCD40_CMD_READ_SERIAL       0x3682
#define SCD40_CMD_REINIT            0x3646

struct scd40 {
    i2c_master_dev_handle_t i2c_dev;
};

esp_err_t scd40_create(const scd40_config_t *config, scd40_t **out) {
    if (!config || !config->i2c_bus || !out) return ESP_ERR_INVALID_ARG;
    uint8_t address = config->i2c_address ? config->i2c_address : SCD40_I2C_ADDR;
    esp_err_t err = i2c_master_probe(config->i2c_bus, address, SENSOR_I2C_TIMEOUT_MS);
    if (err != ESP_OK) return err;

    scd40_t *sensor = calloc(1, sizeof(*sensor));
    if (!sensor) return ESP_ERR_NO_MEM;
    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = address,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : SCD40_I2C_DEFAULT_HZ,
    };
    err = i2c_master_bus_add_device(config->i2c_bus, &dev_cfg, &sensor->i2c_dev);
    if (err != ESP_OK) {
        free(sensor);
        return err;
    }
    *out = sensor;
    return ESP_OK;
}

void scd40_delete(scd40_t *sensor) {
    if (!sensor) return;
    i2c_master_bus_rm_device(sensor->i2c_dev);
    free(sensor);
}

static esp_err_t write_cmd_delay(scd40_t *sensor, uint16_t cmd, uint32_t delay_ms) {
    esp_err_t err = sensirion_write_cmd(sensor->i2c_dev, cmd);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(delay_ms));
    return ESP_OK;
}

static esp_err_t write_cmd_word_delay(scd40_t *sensor, uint16_t cmd, uint16_t word,
                                      uint32_t delay_ms) {
    esp_err_t err = sensirion_write_cmd_words(sensor->i2c_dev, cmd, &word, 1);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(delay_ms));
    return ESP_OK;
}

esp_err_t scd40_start(scd40_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return sensirion_write_cmd(sensor->i2c_dev, SCD40_CMD_START);
}

esp_err_t scd40_start_low_power(scd40_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return sensirion_write_cmd(sensor->i2c_dev, SCD40_CMD_START_LOW_POWER);
}

esp_err_t scd40_stop(scd40_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SCD40_CMD_STOP, 500);
}

esp_err_t scd40_data_ready(scd40_t *sensor, bool *out_ready) {
    if (!sensor || !out_ready) return ESP_ERR_INVALID_ARG;
    uint16_t word;
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SCD40_CMD_DATA_READY, 1, &word, 1);
    if (err != ESP_OK) return err;
    *out_ready = (word & 0x07FF) != 0;
    return ESP_OK;
}

esp_err_t scd40_read(scd40_t *sensor, scd40_data_t *out) {
    if (!sensor || !out) return ESP_ERR_INVALID_ARG;
    uint16_t w[3];
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SCD40_CMD_READ_MEASUREMENT, 1, w, 3);
    if (err != ESP_OK) return err;
    out->co2_ppm     = w[0];
    out->temperature = -45.0f + (175.0f * w[1]) / 65535.0f;
    out->humidity    = (100.0f * w[2]) / 65535.0f;
    return ESP_OK;
}

esp_err_t scd40_set_asc_enabled(scd40_t *sensor, bool enabled) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_word_delay(sensor, SCD40_CMD_SET_ASC, enabled ? 1 : 0, 1);
}

esp_err_t scd40_get_asc_enabled(scd40_t *sensor, bool *out_enabled) {
    if (!sensor || !out_enabled) return ESP_ERR_INVALID_ARG;
    uint16_t word;
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SCD40_CMD_GET_ASC, 1, &word, 1);
    if (err != ESP_OK) return err;
    *out_enabled = word != 0;
    return ESP_OK;
}

esp_err_t scd40_forced_recalibration(scd40_t *sensor, uint16_t target_ppm, int16_t *out_correction) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    uint16_t word;
    esp_err_t err = sensirion_cmd_write_read(sensor->i2c_dev, SCD40_CMD_FORCED_RECAL,
                                             &target_ppm, 1, 400, &word, 1);
    if (err != ESP_OK) return err;
    if (word == 0xFFFF) return ESP_FAIL;  /* chip reports FRC failure */
    if (out_correction) *out_correction = (int16_t)((int32_t)word - 0x8000);
    return ESP_OK;
}

esp_err_t scd40_set_temperature_offset(scd40_t *sensor, float offset_degc) {
    if (!sensor || offset_degc < 0) return ESP_ERR_INVALID_ARG;
    uint16_t raw = (uint16_t)(offset_degc * 65535.0f / 175.0f + 0.5f);
    return write_cmd_word_delay(sensor, SCD40_CMD_SET_TEMP_OFFSET, raw, 1);
}

esp_err_t scd40_set_altitude(scd40_t *sensor, uint16_t meters) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_word_delay(sensor, SCD40_CMD_SET_ALTITUDE, meters, 1);
}

esp_err_t scd40_set_ambient_pressure(scd40_t *sensor, uint32_t pascals) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_word_delay(sensor, SCD40_CMD_SET_PRESSURE, (uint16_t)(pascals / 100), 1);
}

esp_err_t scd40_persist_settings(scd40_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SCD40_CMD_PERSIST_SETTINGS, 800);
}

esp_err_t scd40_read_serial(scd40_t *sensor, uint64_t *out_serial) {
    if (!sensor || !out_serial) return ESP_ERR_INVALID_ARG;
    uint16_t w[3];
    esp_err_t err = sensirion_cmd_read(sensor->i2c_dev, SCD40_CMD_READ_SERIAL, 1, w, 3);
    if (err != ESP_OK) return err;
    *out_serial = ((uint64_t)w[0] << 32) | ((uint64_t)w[1] << 16) | w[2];
    return ESP_OK;
}

esp_err_t scd40_reset(scd40_t *sensor) {
    if (!sensor) return ESP_ERR_INVALID_ARG;
    return write_cmd_delay(sensor, SCD40_CMD_REINIT, 30);
}
