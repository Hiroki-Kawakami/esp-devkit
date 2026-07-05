/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Generic bsp_power provider for boards that sense the battery on an ADC pin
 * through a resistor divider (no fuel-gauge IC). Reports the divided-back
 * terminal voltage and a linear empty..full level; VBUS is optional (a sense
 * GPIO). The board fills adc_battery_config_t with its electrical constants.
 */

#pragma once
#include "bsp_power.h"
#include "esp_adc/adc_oneshot.h"
#include "driver/gpio.h"

typedef struct {
    adc_unit_t    adc_unit;
    adc_channel_t adc_channel;
    adc_atten_t   adc_atten;
    uint32_t      divider_mul;  /*!< battery_mv = pin_mv * divider_mul / divider_div */
    uint32_t      divider_div;
    uint32_t      empty_mv;     /*!< 0% endpoint */
    uint32_t      full_mv;      /*!< 100% endpoint */
    uint8_t       samples;      /*!< averaged reads per query; 0 -> 16 */
    gpio_num_t    vbus_gpio;    /*!< GPIO_NUM_NC -> no VBUS cap */
    bool          vbus_active_high;
} adc_battery_config_t;

esp_err_t adc_battery_create(const adc_battery_config_t *config, bsp_power_t **out_power);
