#include "ip5306.h"

#include <stdlib.h>

#define I2C_TIMEOUT_MS 100
#define REG_BATTERY_LEVEL 0x78

struct ip5306_dev {
    i2c_master_dev_handle_t i2c;
};

typedef struct {
    bsp_power_t base;
    ip5306_handle_t ip5306;
} ip5306_power_t;

esp_err_t ip5306_get_battery_level(ip5306_handle_t handle, uint8_t *out_percent) {
    if (!handle || !out_percent) return ESP_ERR_INVALID_ARG;

    uint8_t value;
    uint8_t reg = REG_BATTERY_LEVEL;
    esp_err_t err = i2c_master_transmit_receive(handle->i2c, &reg, 1, &value, 1,
                                                I2C_TIMEOUT_MS);
    if (err != ESP_OK) return err;

    switch (value & 0xF0) {
        case 0x00: *out_percent = 100; break;
        case 0x80: *out_percent = 75; break;
        case 0xC0: *out_percent = 50; break;
        case 0xE0: *out_percent = 25; break;
        default: *out_percent = 0; break;
    }
    return ESP_OK;
}

esp_err_t ip5306_create(const ip5306_config_t *config, ip5306_handle_t *out_handle) {
    if (!config || !config->i2c_bus || !out_handle) return ESP_ERR_INVALID_ARG;

    ip5306_handle_t handle = calloc(1, sizeof(*handle));
    if (!handle) return ESP_ERR_NO_MEM;

    const i2c_device_config_t device_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = config->i2c_address ? config->i2c_address : IP5306_I2C_ADDR,
        .scl_speed_hz = config->clock_hz ? config->clock_hz : IP5306_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(config->i2c_bus, &device_config, &handle->i2c);
    if (err != ESP_OK) {
        free(handle);
        return err;
    }

    uint8_t level;
    err = ip5306_get_battery_level(handle, &level);
    if (err != ESP_OK) {
        i2c_master_bus_rm_device(handle->i2c);
        free(handle);
        return err;
    }

    *out_handle = handle;
    return ESP_OK;
}

esp_err_t ip5306_destroy(ip5306_handle_t handle) {
    if (!handle) return ESP_ERR_INVALID_ARG;
    esp_err_t err = i2c_master_bus_rm_device(handle->i2c);
    free(handle);
    return err;
}

static esp_err_t power_get_level(bsp_power_t *self, uint8_t *out_percent) {
    ip5306_power_t *power = (ip5306_power_t *)self;
    return ip5306_get_battery_level(power->ip5306, out_percent);
}

static esp_err_t power_deinit(bsp_power_t *self) {
    ip5306_power_t *power = (ip5306_power_t *)self;
    esp_err_t err = ip5306_destroy(power->ip5306);
    free(self);
    return err;
}

esp_err_t ip5306_power_create(ip5306_handle_t handle, bsp_power_t **out_power) {
    if (!handle || !out_power) return ESP_ERR_INVALID_ARG;

    ip5306_power_t *power = calloc(1, sizeof(*power));
    if (!power) return ESP_ERR_NO_MEM;

    power->ip5306 = handle;
    power->base.caps = BSP_POWER_CAP_BATTERY;
    power->base.get_battery_level = power_get_level;
    power->base.deinit = power_deinit;
    *out_power = &power->base;
    return ESP_OK;
}
