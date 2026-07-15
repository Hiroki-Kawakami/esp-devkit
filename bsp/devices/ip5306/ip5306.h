#pragma once

#include "driver/i2c_master.h"
#include "bsp_power.h"

#define IP5306_I2C_ADDR       0x75
#define IP5306_I2C_DEFAULT_HZ 400000

typedef struct ip5306_dev *ip5306_handle_t;

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint16_t i2c_address;
    uint32_t clock_hz;
} ip5306_config_t;

esp_err_t ip5306_create(const ip5306_config_t *config, ip5306_handle_t *out_handle);
esp_err_t ip5306_destroy(ip5306_handle_t handle);
esp_err_t ip5306_get_battery_level(ip5306_handle_t handle, uint8_t *out_percent);
esp_err_t ip5306_power_create(ip5306_handle_t handle, bsp_power_t **out_power);
