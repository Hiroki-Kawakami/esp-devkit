/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bmi270.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "bmi270";

#define REG_CHIP_ID          0x00
#define REG_STATUS           0x03
#define REG_ACC_X_LSB        0x0C
#define REG_GYR_X_LSB        0x12
#define REG_INTERNAL_STATUS  0x21
#define REG_ACC_CONF         0x40
#define REG_ACC_RANGE        0x41
#define REG_GYR_CONF         0x42
#define REG_GYR_RANGE        0x43
#define REG_INIT_CTRL        0x59
#define REG_INIT_ADDR_0      0x5B
#define REG_INIT_DATA        0x5E
#define REG_PWR_CONF         0x7C
#define REG_PWR_CTRL         0x7D
#define REG_CMD              0x7E

#define CHIP_ID              0x24
#define CMD_SOFT_RESET       0xB6
#define STATUS_DRDY_ACC      0x80
#define STATUS_DRDY_GYR      0x40
#define INTERNAL_STATUS_MASK 0x0F
#define INTERNAL_STATUS_OK   0x01
#define PWR_CONF_FUP_EN      0x02
#define PWR_CTRL_GYR_EN      0x02
#define PWR_CTRL_ACC_EN      0x04
#define CONF_FILTER_PERF     0x80
#define CONF_BWP_NORMAL      0x20
#define ODR_CODE_25HZ        0x06
#define DATA_BURST_LEN       (REG_GYR_X_LSB + 6 - REG_STATUS)

#define CONFIG_CHUNK         256
#define I2C_TIMEOUT_MS       100
#define INIT_POLL_MS         10
#define INIT_TIMEOUT_MS      200
#define FIRST_SAMPLE_TIMEOUT_MS 200

#define DEFAULT_ODR_HZ         100
#define DEFAULT_ACCEL_RANGE_G  8
#define DEFAULT_GYRO_RANGE_DPS 2000

#define COUNT_OF(a) (sizeof(a) / sizeof((a)[0]))

extern const uint8_t bmi270_config_file[];
extern const size_t  bmi270_config_file_size;

typedef struct {
    bsp_imu_t               base;
    i2c_master_dev_handle_t i2c_dev;
    float                   accel_scale;
    float                   gyro_scale;
    bsp_vec3_t              accel;
    bsp_vec3_t              gyro;
} bmi270_dev_t;

static const uint16_t k_odr_hz[]           = { 25, 50, 100, 200, 400, 800, 1600 };
static const uint16_t k_accel_range_g[]    = { 2, 4, 8, 16 };
static const uint8_t  k_accel_range_code[] = { 0x00, 0x01, 0x02, 0x03 };
static const uint16_t k_gyro_range_dps[]   = { 125, 250, 500, 1000, 2000 };
static const uint8_t  k_gyro_range_code[]  = { 0x04, 0x03, 0x02, 0x01, 0x00 };

static esp_err_t reg_write(bmi270_dev_t *dev, uint8_t reg, uint8_t value) {
    const uint8_t buf[2] = { reg, value };
    return i2c_master_transmit(dev->i2c_dev, buf, sizeof(buf), I2C_TIMEOUT_MS);
}

static esp_err_t reg_read(bmi270_dev_t *dev, uint8_t reg, uint8_t *data, size_t len) {
    return i2c_master_transmit_receive(dev->i2c_dev, &reg, 1, data, len, I2C_TIMEOUT_MS);
}

static esp_err_t upload_config(bmi270_dev_t *dev) {
    uint8_t buf[1 + CONFIG_CHUNK];
    for (size_t index = 0; index < bmi270_config_file_size; index += CONFIG_CHUNK) {
        size_t len = bmi270_config_file_size - index;
        if (len > CONFIG_CHUNK) len = CONFIG_CHUNK;
        const uint8_t addr[3] = { REG_INIT_ADDR_0, (index >> 1) & 0x0F, (uint8_t)(index >> 5) };
        esp_err_t err = i2c_master_transmit(dev->i2c_dev, addr, sizeof(addr), I2C_TIMEOUT_MS);
        if (err != ESP_OK) return err;
        buf[0] = REG_INIT_DATA;
        memcpy(&buf[1], &bmi270_config_file[index], len);
        err = i2c_master_transmit(dev->i2c_dev, buf, len + 1, I2C_TIMEOUT_MS);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

static esp_err_t chip_init(bmi270_dev_t *dev) {
    uint8_t id = 0;
    esp_err_t err = reg_read(dev, REG_CHIP_ID, &id, 1);
    if (err != ESP_OK) return err;
    if (id != CHIP_ID) {
        ESP_LOGE(TAG, "unexpected chip id 0x%02x", id);
        return ESP_ERR_NOT_FOUND;
    }

    if ((err = reg_write(dev, REG_CMD, CMD_SOFT_RESET)) != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(2) + 1);
    if ((err = reg_write(dev, REG_PWR_CONF, 0x00)) != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(1) + 1);
    if ((err = reg_write(dev, REG_INIT_CTRL, 0x00)) != ESP_OK) return err;
    if ((err = upload_config(dev)) != ESP_OK) return err;
    if ((err = reg_write(dev, REG_INIT_CTRL, 0x01)) != ESP_OK) return err;

    uint8_t status = 0;
    for (int waited = 0; waited < INIT_TIMEOUT_MS; waited += INIT_POLL_MS) {
        vTaskDelay(pdMS_TO_TICKS(INIT_POLL_MS));
        if (reg_read(dev, REG_INTERNAL_STATUS, &status, 1) == ESP_OK &&
            (status & INTERNAL_STATUS_MASK) == INTERNAL_STATUS_OK) {
            break;
        }
    }
    if ((status & INTERNAL_STATUS_MASK) != INTERNAL_STATUS_OK) {
        ESP_LOGE(TAG, "config load failed (internal status 0x%02x)", status);
        return ESP_FAIL;
    }
    return ESP_OK;
}

static esp_err_t chip_enable(bmi270_dev_t *dev) {
    esp_err_t err = reg_write(dev, REG_PWR_CTRL, PWR_CTRL_ACC_EN | PWR_CTRL_GYR_EN);
    if (err != ESP_OK) return err;
    return reg_write(dev, REG_PWR_CONF, PWR_CONF_FUP_EN);
}

static size_t pick_at_least(const uint16_t *table, size_t count, uint16_t value) {
    for (size_t i = 0; i < count; i++) {
        if (table[i] >= value) return i;
    }
    return count - 1;
}

static esp_err_t apply_config(bmi270_dev_t *dev, size_t odr, size_t accel, size_t gyro) {
    uint8_t odr_code = (uint8_t)(ODR_CODE_25HZ + odr);
    esp_err_t err = reg_write(dev, REG_ACC_CONF, CONF_FILTER_PERF | CONF_BWP_NORMAL | odr_code);
    if (err != ESP_OK) return err;
    if ((err = reg_write(dev, REG_ACC_RANGE, k_accel_range_code[accel])) != ESP_OK) return err;
    dev->accel_scale = (float)k_accel_range_g[accel] / 32768.0f;
    if ((err = reg_write(dev, REG_GYR_CONF, CONF_FILTER_PERF | CONF_BWP_NORMAL | odr_code)) != ESP_OK) return err;
    if ((err = reg_write(dev, REG_GYR_RANGE, k_gyro_range_code[gyro])) != ESP_OK) return err;
    dev->gyro_scale = (float)k_gyro_range_dps[gyro] / 32768.0f;

    uint8_t discard[DATA_BURST_LEN];
    return reg_read(dev, REG_STATUS, discard, sizeof(discard));
}

static esp_err_t bmi270_configure(bsp_imu_t *self, const bsp_imu_config_t *config,
                                  bsp_imu_config_t *out_actual) {
    bmi270_dev_t *dev = (bmi270_dev_t *)self;
    size_t odr   = pick_at_least(k_odr_hz, COUNT_OF(k_odr_hz), config->odr_hz);
    size_t accel = pick_at_least(k_accel_range_g, COUNT_OF(k_accel_range_g), config->accel_range_g);
    size_t gyro  = pick_at_least(k_gyro_range_dps, COUNT_OF(k_gyro_range_dps), config->gyro_range_dps);

    uint8_t pwr_ctrl = 0;
    esp_err_t err = reg_read(dev, REG_PWR_CTRL, &pwr_ctrl, 1);
    if (err != ESP_OK) return err;
    /* Reconfiguring a running sensor keeps raising drdy with data still at the old
     * range for up to ~300 ms (seen at 1600Hz/2g -> 25Hz/16g); pausing it avoids that. */
    const uint8_t sensors = pwr_ctrl & (PWR_CTRL_ACC_EN | PWR_CTRL_GYR_EN);
    if (sensors && (err = reg_write(dev, REG_PWR_CTRL, pwr_ctrl & ~sensors)) != ESP_OK) return err;

    err = apply_config(dev, odr, accel, gyro);
    if (sensors) {
        esp_err_t restore = reg_write(dev, REG_PWR_CTRL, pwr_ctrl);
        if (err == ESP_OK) err = restore;
    }
    if (err != ESP_OK) return err;

    *out_actual = (bsp_imu_config_t){
        .odr_hz         = k_odr_hz[odr],
        .accel_range_g  = k_accel_range_g[accel],
        .gyro_range_dps = k_gyro_range_dps[gyro],
    };
    return ESP_OK;
}

static float le16(const uint8_t *p, float scale) {
    return (float)(int16_t)(p[0] | (p[1] << 8)) * scale;
}

static esp_err_t fetch(bmi270_dev_t *dev, uint8_t *out_status) {
    uint8_t buf[DATA_BURST_LEN];
    esp_err_t err = reg_read(dev, REG_STATUS, buf, sizeof(buf));
    if (err != ESP_OK) return err;

    const uint8_t status = buf[0];
    if (status & STATUS_DRDY_ACC) {
        const uint8_t *a = &buf[REG_ACC_X_LSB - REG_STATUS];
        dev->accel = (bsp_vec3_t){ le16(&a[0], dev->accel_scale), le16(&a[2], dev->accel_scale), le16(&a[4], dev->accel_scale) };
    }
    if (status & STATUS_DRDY_GYR) {
        const uint8_t *g = &buf[REG_GYR_X_LSB - REG_STATUS];
        dev->gyro = (bsp_vec3_t){ le16(&g[0], dev->gyro_scale), le16(&g[2], dev->gyro_scale), le16(&g[4], dev->gyro_scale) };
    }
    *out_status = status;
    return ESP_OK;
}

static esp_err_t bmi270_read(bsp_imu_t *self, bsp_vec3_t *accel, bsp_vec3_t *gyro, bool *out_updated) {
    bmi270_dev_t *dev = (bmi270_dev_t *)self;
    uint8_t status = 0;
    esp_err_t err = fetch(dev, &status);
    if (err != ESP_OK) return err;
    *accel = dev->accel;
    *gyro  = dev->gyro;
    *out_updated = (status & (STATUS_DRDY_ACC | STATUS_DRDY_GYR)) != 0;
    return ESP_OK;
}

static esp_err_t wait_first_accel(bmi270_dev_t *dev) {
    for (int waited = 0; waited < FIRST_SAMPLE_TIMEOUT_MS; waited += INIT_POLL_MS) {
        vTaskDelay(pdMS_TO_TICKS(INIT_POLL_MS));
        uint8_t status = 0;
        esp_err_t err = fetch(dev, &status);
        if (err != ESP_OK) return err;
        if (status & STATUS_DRDY_ACC) return ESP_OK;
    }
    return ESP_ERR_TIMEOUT;
}

static esp_err_t bmi270_deinit(bsp_imu_t *self) {
    bmi270_dev_t *dev = (bmi270_dev_t *)self;
    reg_write(dev, REG_PWR_CTRL, 0x00);
    i2c_master_bus_rm_device(dev->i2c_dev);
    free(dev);
    return ESP_OK;
}

esp_err_t bmi270_imu_create(const bmi270_config_t *config, bsp_imu_t **out_imu) {
    if (!config || !config->i2c_bus || !out_imu) return ESP_ERR_INVALID_ARG;

    bmi270_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;

    const i2c_device_config_t dev_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = config->i2c_address ? config->i2c_address : BMI270_I2C_ADDR,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : BMI270_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(config->i2c_bus, &dev_config, &dev->i2c_dev);
    if (err != ESP_OK) {
        free(dev);
        return err;
    }

    const bsp_imu_config_t initial = {
        .odr_hz         = config->config.odr_hz ? config->config.odr_hz : DEFAULT_ODR_HZ,
        .accel_range_g  = config->config.accel_range_g ? config->config.accel_range_g : DEFAULT_ACCEL_RANGE_G,
        .gyro_range_dps = config->config.gyro_range_dps ? config->config.gyro_range_dps : DEFAULT_GYRO_RANGE_DPS,
    };
    if ((err = chip_init(dev)) != ESP_OK ||
        (err = bmi270_configure(&dev->base, &initial, &dev->base.config)) != ESP_OK ||
        (err = chip_enable(dev)) != ESP_OK ||
        (err = wait_first_accel(dev)) != ESP_OK) {
        i2c_master_bus_rm_device(dev->i2c_dev);
        free(dev);
        return err;
    }

    dev->base.read      = bmi270_read;
    dev->base.configure = bmi270_configure;
    dev->base.deinit    = bmi270_deinit;
    dev->base.caps      = BSP_IMU_CAP_ACCEL | BSP_IMU_CAP_GYRO;
    dev->base.axis_map  = config->axis_map;
    *out_imu = &dev->base;
    return ESP_OK;
}
