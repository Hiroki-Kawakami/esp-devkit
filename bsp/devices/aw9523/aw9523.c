/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "aw9523.h"
#include "esp_log.h"
#include <stdlib.h>

static const char *TAG = "aw9523";

#define I2C_TIMEOUT_MS  100

/* Registers. Port 0 uses the low byte / even reg, Port 1 the high byte. */
#define REG_INPUT0    0x00
#define REG_OUTPUT0   0x02
#define REG_CONFIG0   0x04   /* 1 = input, 0 = output */
#define REG_INT0      0x06   /* interrupt mask: 1 = disabled, 0 = enabled */
#define REG_ID        0x10   /* reads 0x23 */
#define REG_CTL       0x11   /* bit4: P0 push-pull (1) vs open-drain (0) */
#define REG_LEDMODE0  0x12   /* 1 = GPIO mode, 0 = constant-current LED */
#define REG_SOFT_RST  0x7F

struct aw9523_state {
    i2c_master_dev_handle_t i2c;
    uint16_t direction;   /* 1 = output */
    uint16_t output;
};

static esp_err_t write_reg(aw9523_t aw, uint8_t reg, uint8_t val) {
    uint8_t buf[2] = { reg, val };
    return i2c_master_transmit(aw->i2c, buf, sizeof(buf), I2C_TIMEOUT_MS);
}

static esp_err_t read_reg(aw9523_t aw, uint8_t reg, uint8_t *val) {
    return i2c_master_transmit_receive(aw->i2c, &reg, 1, val, 1, I2C_TIMEOUT_MS);
}

esp_err_t aw9523_init(i2c_master_bus_handle_t i2c_bus, uint8_t address,
                      const aw9523_pin_config_t config[16], aw9523_t *out) {
    if (!i2c_bus || !config || !out) return ESP_ERR_INVALID_ARG;

    struct aw9523_state *aw = calloc(1, sizeof(*aw));
    if (!aw) return ESP_ERR_NO_MEM;

    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = address,
        .scl_speed_hz    = AW9523_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(i2c_bus, &dev_cfg, &aw->i2c);
    if (err != ESP_OK) { free(aw); return err; }

    uint16_t dir = 0, outv = 0;    /* 1<<pin here means output */
    uint16_t intmask = 0xFFFF;     /* 1 = INT disabled; unmask requested inputs */
    for (int i = 0; i < 16; i++) {
        if (config[i].mode == AW9523_PIN_MODE_OUTPUT) {
            dir |= (uint16_t)(1u << i);
            if (config[i].initial_value) outv |= (uint16_t)(1u << i);
        } else if (config[i].mode == AW9523_PIN_MODE_INPUT && config[i].interrupt) {
            intmask &= (uint16_t)~(1u << i);
        }
    }

    err = write_reg(aw, REG_SOFT_RST, 0x00);
    if (err == ESP_OK) {
        uint8_t id;
        if (read_reg(aw, REG_ID, &id) != ESP_OK) {
            ESP_LOGE(TAG, "not responding on 0x%02x", address);
            err = ESP_ERR_NOT_FOUND;
        }
    }
    if (err == ESP_OK) err = write_reg(aw, REG_LEDMODE0,     0xFF);  /* P0 all GPIO */
    if (err == ESP_OK) err = write_reg(aw, REG_LEDMODE0 + 1, 0xFF);  /* P1 all GPIO */
    if (err == ESP_OK) err = write_reg(aw, REG_CTL, 0x10);           /* P0 push-pull */
    /* drive outputs before switching direction so no glitch on enable */
    if (err == ESP_OK) err = write_reg(aw, REG_OUTPUT0,     (uint8_t)(outv & 0xFF));
    if (err == ESP_OK) err = write_reg(aw, REG_OUTPUT0 + 1, (uint8_t)(outv >> 8));
    if (err == ESP_OK) err = write_reg(aw, REG_CONFIG0,     (uint8_t)(~dir & 0xFF));
    if (err == ESP_OK) err = write_reg(aw, REG_CONFIG0 + 1, (uint8_t)(~(dir >> 8) & 0xFF));
    if (err == ESP_OK) err = write_reg(aw, REG_INT0,     (uint8_t)(intmask & 0xFF));
    if (err == ESP_OK) err = write_reg(aw, REG_INT0 + 1, (uint8_t)(intmask >> 8));
    if (err != ESP_OK) {
        i2c_master_bus_rm_device(aw->i2c);
        free(aw);
        return err;
    }

    aw->direction = dir;
    aw->output    = outv;
    *out = aw;
    ESP_LOGI(TAG, "AW9523B ready");
    return ESP_OK;
}

esp_err_t aw9523_deinit(aw9523_t aw) {
    if (!aw) return ESP_ERR_INVALID_ARG;
    esp_err_t err = i2c_master_bus_rm_device(aw->i2c);
    free(aw);
    return err;
}

esp_err_t aw9523_set_output(aw9523_t aw, uint8_t pin, bool value) {
    if (!aw || pin >= 16) return ESP_ERR_INVALID_ARG;
    uint16_t mask = (uint16_t)(1u << pin);
    if (!(aw->direction & mask)) return ESP_ERR_INVALID_STATE;
    if (((aw->output & mask) != 0) == value) return ESP_OK;
    if (value) aw->output |= mask; else aw->output &= (uint16_t)~mask;
    uint8_t reg = REG_OUTPUT0 + (pin >= 8 ? 1 : 0);
    uint8_t byte = (uint8_t)(pin >= 8 ? aw->output >> 8 : aw->output & 0xFF);
    return write_reg(aw, reg, byte);
}

esp_err_t aw9523_get_input(aw9523_t aw, uint8_t pin, bool *value) {
    if (!aw || pin >= 16 || !value) return ESP_ERR_INVALID_ARG;
    uint16_t mask = (uint16_t)(1u << pin);
    if (aw->direction & mask) { *value = (aw->output & mask) != 0; return ESP_OK; }
    uint8_t v;
    esp_err_t err = read_reg(aw, REG_INPUT0 + (pin >= 8 ? 1 : 0), &v);
    if (err != ESP_OK) return err;
    *value = (v & (uint8_t)(1u << (pin & 7))) != 0;
    return ESP_OK;
}

esp_err_t aw9523_read_inputs(aw9523_t aw, uint16_t *out_state) {
    if (!aw) return ESP_ERR_INVALID_ARG;
    /* Read each input port with its own transaction: the AW9523B clears a port's
     * INT latch on a read of that register, and a 2-byte burst does not. */
    uint8_t p0, p1;
    esp_err_t err = read_reg(aw, REG_INPUT0,     &p0);
    if (err != ESP_OK) return err;
    err = read_reg(aw, REG_INPUT0 + 1, &p1);
    if (err != ESP_OK) return err;
    if (out_state) *out_state = (uint16_t)p0 | ((uint16_t)p1 << 8);
    return ESP_OK;
}
