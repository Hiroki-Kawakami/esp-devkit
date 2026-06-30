/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "pi4io.h"
#include "esp_log.h"
#include <stdlib.h>

static const char *TAG = "pi4io";

typedef enum {
    PI4IO_REG_CHIP_RESET = 0x01,
    PI4IO_REG_IO_DIR     = 0x03,
    PI4IO_REG_OUT_SET    = 0x05,
    PI4IO_REG_OUT_H_IM   = 0x07,
    PI4IO_REG_IN_DEF_STA = 0x09,
    PI4IO_REG_PULL_EN    = 0x0B,
    PI4IO_REG_PULL_SEL   = 0x0D,
    PI4IO_REG_IN_STA     = 0x0F,
    PI4IO_REG_INT_MASK   = 0x11,
    PI4IO_REG_IRQ_STA    = 0x13,
} pi4io_reg_t;

struct pi4io_state {
    i2c_master_dev_handle_t device;
    uint8_t direction;
    uint8_t output;
};

static esp_err_t pi4io_write_reg(pi4io_t pi4io, pi4io_reg_t reg, uint8_t value) {
    uint8_t data[2] = { (uint8_t)reg, value };
    return i2c_master_transmit(pi4io->device, data, 2, -1);
}

static esp_err_t pi4io_read_reg(pi4io_t pi4io, pi4io_reg_t reg, uint8_t *value) {
    uint8_t reg_addr = reg;
    return i2c_master_transmit_receive(pi4io->device, &reg_addr, 1, value, 1, -1);
}

esp_err_t pi4io_init(i2c_master_bus_handle_t i2c_bus, uint8_t address, pi4io_pin_config_t config[8], pi4io_t *pi4io) {
    esp_err_t ret;

    struct pi4io_state *state = calloc(1, sizeof(struct pi4io_state));
    if (state == NULL) return ESP_ERR_NO_MEM;

    i2c_device_config_t dev_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = address,
        .scl_speed_hz    = 400000,
    };
    ret = i2c_master_bus_add_device(i2c_bus, &dev_config, &state->device);
    if (ret != ESP_OK) {
        free(state);
        return ret;
    }

    uint8_t io_dir = 0, io_hm = 0x00, pull_sel = 0, pull_en = 0;
    uint8_t in_def_sta = 0, int_mask = 0, out_set = 0;

    for (int i = 0; i < 8; i++) {
        uint8_t mask = 1 << i;
        pi4io_pin_config_t *pin = &config[i];
        switch (pin->mode) {
            case PI4IO_PIN_MODE_OUTPUT:
                io_dir |= mask;
                if (pin->initial_value) out_set |= mask;
                if (pin->pull == PI4IO_PULL_UP) pull_sel |= mask;
                if (pin->pull != PI4IO_PULL_NONE) pull_en |= mask;
                int_mask |= mask;
                break;
            case PI4IO_PIN_MODE_INPUT:
                if (pin->initial_value) in_def_sta |= mask;
                if (pin->pull == PI4IO_PULL_UP) pull_sel |= mask;
                if (pin->pull != PI4IO_PULL_NONE) pull_en |= mask;
                if (!pin->interrupt) int_mask |= mask;
                break;
            case PI4IO_PIN_MODE_NONE:
            default:
                int_mask |= mask;
                break;
        }
    }

    ret = pi4io_write_reg(state, PI4IO_REG_CHIP_RESET, 0xFF);
    if (ret != ESP_OK) goto err;
    uint8_t dummy;
    ret = pi4io_read_reg(state, PI4IO_REG_CHIP_RESET, &dummy);
    if (ret != ESP_OK) goto err;

    if (io_dir != 0x00 && (ret = pi4io_write_reg(state, PI4IO_REG_IO_DIR, io_dir)) != ESP_OK) goto err;
    if (io_hm != 0xFF && (ret = pi4io_write_reg(state, PI4IO_REG_OUT_H_IM, io_hm)) != ESP_OK) goto err;
    if (pull_sel != 0x00 && (ret = pi4io_write_reg(state, PI4IO_REG_PULL_SEL, pull_sel)) != ESP_OK) goto err;
    if (pull_en != 0xFF && (ret = pi4io_write_reg(state, PI4IO_REG_PULL_EN, pull_en)) != ESP_OK) goto err;
    if (in_def_sta != 0x00 && (ret = pi4io_write_reg(state, PI4IO_REG_IN_DEF_STA, in_def_sta)) != ESP_OK) goto err;
    if (int_mask != 0x00 && (ret = pi4io_write_reg(state, PI4IO_REG_INT_MASK, int_mask)) != ESP_OK) goto err;
    if (out_set != 0x00 && (ret = pi4io_write_reg(state, PI4IO_REG_OUT_SET, out_set)) != ESP_OK) goto err;

    state->direction = io_dir;
    state->output    = out_set;
    *pi4io = state;
    return ESP_OK;

err:
    i2c_master_bus_rm_device(state->device);
    free(state);
    return ret;
}

esp_err_t pi4io_deinit(pi4io_t pi4io) {
    if (pi4io == NULL) return ESP_ERR_INVALID_ARG;
    esp_err_t ret = i2c_master_bus_rm_device(pi4io->device);
    free(pi4io);
    return ret;
}

esp_err_t pi4io_set_output(pi4io_t pi4io, uint8_t pin, bool value) {
    if (pi4io == NULL || pin >= 8) return ESP_ERR_INVALID_ARG;
    uint8_t mask = 1 << pin;
    if ((pi4io->direction & mask) == 0) {
        ESP_LOGE(TAG, "Pin %d is not configured as output", pin);
        return ESP_ERR_INVALID_STATE;
    }
    if (((pi4io->output & mask) != 0) == value) return ESP_OK;
    if (value) pi4io->output |= mask;
    else       pi4io->output &= ~mask;
    return pi4io_write_reg(pi4io, PI4IO_REG_OUT_SET, pi4io->output);
}

esp_err_t pi4io_get_input(pi4io_t pi4io, uint8_t pin, bool *value) {
    if (pi4io == NULL || pin >= 8 || value == NULL) return ESP_ERR_INVALID_ARG;
    uint8_t mask = 1 << pin;
    if (pi4io->direction & mask) {
        *value = (pi4io->output & mask) != 0;
        return ESP_OK;
    }
    uint8_t input;
    esp_err_t ret = pi4io_read_reg(pi4io, PI4IO_REG_IN_STA, &input);
    if (ret != ESP_OK) return ret;
    *value = (input & mask) != 0;
    return ESP_OK;
}

esp_err_t pi4io_get_all_inputs(pi4io_t pi4io, uint8_t *value) {
    if (pi4io == NULL || value == NULL) return ESP_ERR_INVALID_ARG;
    return pi4io_read_reg(pi4io, PI4IO_REG_IN_STA, value);
}

esp_err_t pi4io_get_irq_status(pi4io_t pi4io, uint8_t *status) {
    if (pi4io == NULL || status == NULL) return ESP_ERR_INVALID_ARG;
    return pi4io_read_reg(pi4io, PI4IO_REG_IRQ_STA, status);
}
