/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * BM8563 I2C RTC driver. The board passes an initialized I2C bus handle; the
 * driver attaches as a device on it and does not own the bus. Returns a
 * bsp_rtc_t for bsp_rtc_set_active().
 */

#pragma once
#include "bsp_types.h"
#include "bsp_rtc.h"
#include "driver/i2c_master.h"
#include "driver/gpio.h"

#define BM8563_I2C_ADDR        0x51
#define BM8563_I2C_DEFAULT_HZ  (400 * 1000)

typedef struct {
    i2c_master_bus_handle_t i2c_bus;      /*!< board-initialized bus, not owned */
    uint8_t                 i2c_address;  /*!< 0 -> BM8563_I2C_ADDR */
    uint32_t                clock_hz;     /*!< 0 -> BM8563_I2C_DEFAULT_HZ */
    gpio_num_t              int_io;       /*!< INT, active LOW; GPIO_NUM_NC to skip */
} bm8563_config_t;

esp_err_t bm8563_rtc_create(const bm8563_config_t *config, bsp_rtc_t **out_rtc);
