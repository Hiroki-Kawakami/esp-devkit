/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Register sequences ported from LovyanGFX (Panel_M5HDMI::HDMI_Trans),
 * Copyright (c) lovyan03, licensed under the FreeBSD License.
 */

#include "lt8618sx.h"
#include <stdlib.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "lt8618sx";

#define REG_PAGE       0xFF
#define PLL_LOCK_RETRY 8

struct lt8618sx_state {
    i2c_master_dev_handle_t device;
};

/* Page 0x80 unlocks the ID registers (0xEE=0x01), which live at 0x00..0x02. */
static const uint8_t SEQ_ID_UNLOCK[] = { 0xff, 0x80, 0xee, 0x01 };

static const uint8_t SEQ_RESET[] = {
    0xff, 0x81, 0x30, 0x00, 0x02, 0x66, 0x0a, 0x06, 0x15, 0x06, 0x4e, 0xa8,
    0xff, 0x80, 0xee, 0x01, 0x11, 0x00, 0x13, 0xf1, 0x13, 0xf9, 0x0a, 0x80,
    0xff, 0x82, 0x1b, 0x77, 0x1c, 0xec, 0x45, 0x00, 0x4f, 0x40, 0x50, 0x00,
    0x47, 0x07,
};

/* Audio path (48 kHz I2S) followed by the video input setup. */
static const uint8_t SEQ_AUDIO_VIDEO[] = {
    0xff, 0x82, 0xd6, 0x8e, 0xd7, 0x04,
    0xff, 0x84, 0x06, 0x08, 0x07, 0x10, 0x09, 0x00, 0x0f, 0x2b, 0x34, 0xd5,
    0x35, 0x00, 0x36, 0x18, 0x37, 0x00, 0x3c, 0x21,
    0xff, 0x82, 0xde, 0x00, 0xde, 0xc0,
    0xff, 0x81, 0x23, 0x40, 0x24, 0x64, 0x26, 0x55, 0x29, 0x04, 0x4d, 0x00,
    0x27, 0x60, 0x28, 0x00, 0x25, 0x01, 0x2c, 0x94, 0x2d, 0x99,
};

/* Extra tuning the U3 die (third ID byte 0xE2) needs, on page 0x81. */
static const uint8_t SEQ_U3[] = {
    0x4d, 0x09, 0x27, 0x66, 0x28, 0x88, 0x2a, 0x00, 0x2a, 0x20, 0x25, 0x00,
    0x2c, 0x9e, 0x2d, 0x99,
};

/* One PLL start attempt; leaves page 0x82 selected so the lock status can be
 * read back from 0x15 / 0xEA / 0xEB. */
static const uint8_t SEQ_PLL_START[] = {
    0xff, 0x80, 0x16, 0xf1, 0x18, 0xdc, 0x18, 0xfc, 0x16, 0xf3, 0x16, 0xe3,
    0x16, 0xf3, 0xff, 0x82,
};

/* Applied once the PLL reports locked: TX channel setup and output enable. */
static const uint8_t SEQ_OUTPUT_ENABLE[] = {
    0xb9, 0x00,
    0xff, 0x84, 0x43, 0x31, 0x44, 0x10, 0x45, 0x2a, 0x47, 0x04, 0x10, 0x2c,
    0x12, 0x64, 0x3d, 0x0a,
    0xff, 0x80, 0x11, 0x00, 0x13, 0xf1, 0x13, 0xf9,
    0xff, 0x81, 0x31, 0x44, 0x32, 0x4a, 0x33, 0x0b, 0x34, 0x00, 0x35, 0x00,
    0x36, 0x00, 0x37, 0x44, 0x3f, 0x0f, 0x40, 0xa0, 0x41, 0xa0, 0x42, 0xa0,
    0x43, 0xa0, 0x44, 0xa0, 0x30, 0xea,
};

static esp_err_t write_reg(lt8618sx_t dev, uint8_t reg, uint8_t value) {
    const uint8_t data[2] = { reg, value };
    esp_err_t err = ESP_OK;
    for (int retry = 0; retry < 4; retry++) {
        err = i2c_master_transmit(dev->device, data, sizeof(data), -1);
        if (err == ESP_OK) return ESP_OK;
        vTaskDelay(pdMS_TO_TICKS(1));
    }
    ESP_LOGW(TAG, "write reg %02x = %02x: %s", reg, value, esp_err_to_name(err));
    return err;
}

/* The chip NACKs a repeated START, so the register address has to be written as
 * its own transaction (terminated by a STOP) before the read. */
static esp_err_t read_reg(lt8618sx_t dev, uint8_t reg, uint8_t *out) {
    esp_err_t err = i2c_master_transmit(dev->device, &reg, 1, -1);
    if (err != ESP_OK) return err;
    return i2c_master_receive(dev->device, out, 1, -1);
}

static esp_err_t write_seq(lt8618sx_t dev, const uint8_t *pairs, size_t len) {
    for (size_t i = 0; i + 1 < len; i += 2) {
        esp_err_t err = write_reg(dev, pairs[i], pairs[i + 1]);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

static esp_err_t update_reg(lt8618sx_t dev, uint8_t reg, uint8_t mask) {
    uint8_t value = 0;
    esp_err_t err = read_reg(dev, reg, &value);
    if (err != ESP_OK) return err;
    return write_reg(dev, reg, value & mask);
}

esp_err_t lt8618sx_read_chip_id(lt8618sx_t dev, uint8_t out_id[3]) {
    if (!dev || !out_id) return ESP_ERR_INVALID_ARG;
    esp_err_t err = write_seq(dev, SEQ_ID_UNLOCK, sizeof(SEQ_ID_UNLOCK));
    if (err != ESP_OK) return err;
    for (uint8_t reg = 0; reg < 3; reg++) {
        err = read_reg(dev, reg, &out_id[reg]);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

esp_err_t lt8618sx_reset(lt8618sx_t dev) {
    if (!dev) return ESP_ERR_INVALID_ARG;
    return write_seq(dev, SEQ_RESET, sizeof(SEQ_RESET));
}

esp_err_t lt8618sx_init(lt8618sx_t dev) {
    if (!dev) return ESP_ERR_INVALID_ARG;

    uint8_t id[3] = { 0, 0, 0 };
    esp_err_t err = lt8618sx_read_chip_id(dev, id);
    if (err != ESP_OK) return err;

    err = write_seq(dev, SEQ_AUDIO_VIDEO, sizeof(SEQ_AUDIO_VIDEO));
    if (err != ESP_OK) return err;
    if ((err = update_reg(dev, 0x2b, 0xfd)) != ESP_OK) return err;
    if ((err = update_reg(dev, 0x2e, 0xfe)) != ESP_OK) return err;

    if (id[2] == 0xe2 && (err = write_seq(dev, SEQ_U3, sizeof(SEQ_U3))) != ESP_OK) {
        return err;
    }

    for (int retry = 0; retry < PLL_LOCK_RETRY; retry++) {
        err = write_seq(dev, SEQ_PLL_START, sizeof(SEQ_PLL_START));
        if (err != ESP_OK) return err;

        uint8_t status = 0, value = 0, done = 0;
        if (read_reg(dev, 0x15, &status) != ESP_OK) continue;
        if (read_reg(dev, 0xea, &value) != ESP_OK) continue;
        if (read_reg(dev, 0xeb, &done) != ESP_OK) continue;
        if (!(status & 0x80) || !(done & 0x80) || value == 0xff) continue;

        return write_seq(dev, SEQ_OUTPUT_ENABLE, sizeof(SEQ_OUTPUT_ENABLE));
    }

    ESP_LOGE(TAG, "TX PLL did not lock");
    return ESP_ERR_INVALID_STATE;
}

esp_err_t lt8618sx_create(i2c_master_bus_handle_t i2c_bus, uint8_t address, lt8618sx_t *out) {
    if (!i2c_bus || !out) return ESP_ERR_INVALID_ARG;

    struct lt8618sx_state *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;

    const i2c_device_config_t dev_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = address ? address : LT8618SX_I2C_ADDRESS,
        .scl_speed_hz    = LT8618SX_I2C_CLOCK_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(i2c_bus, &dev_config, &dev->device);
    if (err != ESP_OK) {
        free(dev);
        return err;
    }

    *out = dev;
    return ESP_OK;
}

esp_err_t lt8618sx_delete(lt8618sx_t dev) {
    if (!dev) return ESP_OK;
    if (dev->device) i2c_master_bus_rm_device(dev->device);
    free(dev);
    return ESP_OK;
}
