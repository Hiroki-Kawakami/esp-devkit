/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 Keyboard (I2C, STM32 firmware with HID / normal / character
 * modes and two RGB LEDs).
 */

#pragma once
#include "bsp_keyboard.h"
#include "bsp_led.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define TAB5_KEYBOARD_I2C_ADDR        0x6D
#define TAB5_KEYBOARD_I2C_DEFAULT_HZ  (400 * 1000)

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t                 i2c_address;  /* 0 -> TAB5_KEYBOARD_I2C_ADDR */
    uint32_t                clock_hz;     /* 0 -> TAB5_KEYBOARD_I2C_DEFAULT_HZ */
    gpio_num_t              int_io;       /* active LOW; GPIO_NUM_NC -> polled */
} tab5_keyboard_config_t;

/* The LED provider counts 2 only while the keyboard is connected. */
esp_err_t tab5_keyboard_create(const tab5_keyboard_config_t *config,
                               bsp_keyboard_t **out_keyboard, bsp_led_t **out_led);

#ifdef __cplusplus
}
#endif
