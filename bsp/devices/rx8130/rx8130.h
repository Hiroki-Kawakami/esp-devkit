/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * RX8130CE I2C RTC driver. Attaches to a board-initialized bus it does not own
 * and returns a bsp_rtc_t for bsp_rtc_set_active().
 */

#pragma once
#include "bsp_types.h"
#include "bsp_rtc.h"
#include "driver/i2c_master.h"
#include "driver/gpio.h"

#define RX8130_I2C_ADDR        0x32
#define RX8130_I2C_DEFAULT_HZ  (400 * 1000)

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t                 i2c_address;     /*!< 0 -> RX8130_I2C_ADDR */
    uint32_t                clock_hz;        /*!< 0 -> RX8130_I2C_DEFAULT_HZ */
    gpio_num_t              int_io;          /*!< /IRQ, active LOW pulse; GPIO_NUM_NC to skip */
    bool                    charge_backup;   /*!< charge VBAT from VDD; only for a rechargeable backup cell */
    uint32_t                timer_min_ms;    /*!< shorter timer_start -> ESP_ERR_NOT_SUPPORTED; 0 -> no limit */
} rx8130_config_t;

esp_err_t rx8130_rtc_create(const rx8130_config_t *config, bsp_rtc_t **out_rtc);
