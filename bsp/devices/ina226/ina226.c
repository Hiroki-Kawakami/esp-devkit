/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ina226.h"
#include <stdlib.h>
#include "esp_log.h"

static const char *TAG = "ina226";

#define I2C_TIMEOUT_MS  100

#define REG_CONFIG      0x00
#define REG_SHUNT_VOLTAGE 0x01
#define REG_BUS_VOLTAGE 0x02
#define REG_MANUF_ID    0xFE

#define MANUF_ID        0x5449
#define CONFIG_RESERVED (1u << 14)
#define CONFIG_AVG_16   (2u << 9)
#define CONFIG_VBUSCT_1100US (4u << 6)
#define CONFIG_VSHCT_1100US  (4u << 3)
#define CONFIG_MODE_CONT     7u
#define BUS_UV_PER_LSB  1250u
#define SHUNT_NV_PER_LSB 2500

struct ina226_dev {
    i2c_master_dev_handle_t i2c;
    uint32_t shunt_mohm;
};

static esp_err_t write_reg(ina226_handle_t h, uint8_t reg, uint16_t val) {
    uint8_t buf[3] = { reg, (uint8_t)(val >> 8), (uint8_t)val };
    return i2c_master_transmit(h->i2c, buf, sizeof(buf), I2C_TIMEOUT_MS);
}

static esp_err_t read_reg(ina226_handle_t h, uint8_t reg, uint16_t *val) {
    uint8_t buf[2];
    esp_err_t err = i2c_master_transmit_receive(h->i2c, &reg, 1, buf, sizeof(buf), I2C_TIMEOUT_MS);
    if (err != ESP_OK) return err;
    *val = (uint16_t)((buf[0] << 8) | buf[1]);
    return ESP_OK;
}

esp_err_t ina226_create(const ina226_config_t *cfg, ina226_handle_t *out_handle) {
    if (!cfg || !cfg->i2c_bus || !cfg->i2c_address || !out_handle) return ESP_ERR_INVALID_ARG;

    ina226_handle_t h = calloc(1, sizeof(*h));
    if (!h) return ESP_ERR_NO_MEM;

    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = cfg->i2c_address,
        .scl_speed_hz    = cfg->clock_hz ? cfg->clock_hz : INA226_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(cfg->i2c_bus, &dev_cfg, &h->i2c);
    if (err != ESP_OK) {
        free(h);
        return err;
    }

    uint16_t id = 0;
    err = read_reg(h, REG_MANUF_ID, &id);
    if (err == ESP_OK && id != MANUF_ID) {
        ESP_LOGE(TAG, "unexpected manufacturer id 0x%04x", id);
        err = ESP_ERR_NOT_FOUND;
    }
    if (err == ESP_OK) {
        err = write_reg(h, REG_CONFIG, (uint16_t)(CONFIG_RESERVED | CONFIG_AVG_16 | CONFIG_VBUSCT_1100US
                                                  | CONFIG_VSHCT_1100US | CONFIG_MODE_CONT));
    }
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "init at 0x%02x: %s", cfg->i2c_address, esp_err_to_name(err));
        i2c_master_bus_rm_device(h->i2c);
        free(h);
        return err;
    }

    h->shunt_mohm = cfg->shunt_mohm;
    *out_handle = h;
    return ESP_OK;
}

esp_err_t ina226_destroy(ina226_handle_t h) {
    if (!h) return ESP_ERR_INVALID_ARG;
    esp_err_t err = i2c_master_bus_rm_device(h->i2c);
    free(h);
    return err;
}

esp_err_t ina226_get_bus_voltage_mv(ina226_handle_t h, uint32_t *out_mv) {
    if (!h || !out_mv) return ESP_ERR_INVALID_ARG;
    uint16_t raw;
    esp_err_t err = read_reg(h, REG_BUS_VOLTAGE, &raw);
    if (err != ESP_OK) return err;
    *out_mv = (uint32_t)raw * BUS_UV_PER_LSB / 1000u;
    return ESP_OK;
}

esp_err_t ina226_get_current_ma(ina226_handle_t h, int32_t *out_ma) {
    if (!h || !out_ma) return ESP_ERR_INVALID_ARG;
    if (!h->shunt_mohm) return ESP_ERR_INVALID_STATE;
    uint16_t raw;
    esp_err_t err = read_reg(h, REG_SHUNT_VOLTAGE, &raw);
    if (err != ESP_OK) return err;
    *out_ma = (int32_t)(int16_t)raw * SHUNT_NV_PER_LSB / 1000 / (int32_t)h->shunt_mohm;
    return ESP_OK;
}
