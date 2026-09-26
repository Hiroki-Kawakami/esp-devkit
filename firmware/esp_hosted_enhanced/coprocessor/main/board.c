/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "board.h"

#include "driver/gpio.h"
#include "sdkconfig.h"

#if CONFIG_COPROCESSOR_BOARD_XIAO_ESP32C6

#define XIAO_RF_SWITCH_EN   GPIO_NUM_3
#define XIAO_ANT_SELECT     GPIO_NUM_14

void board_init(void) {
    gpio_set_direction(XIAO_RF_SWITCH_EN, GPIO_MODE_OUTPUT);
    gpio_set_level(XIAO_RF_SWITCH_EN, 0);
    gpio_set_direction(XIAO_ANT_SELECT, GPIO_MODE_OUTPUT);
    gpio_set_level(XIAO_ANT_SELECT, 0);
}

sdio_slave_timing_t board_sdio_timing(void) {
    return SDIO_SLAVE_TIMING_NSEND_PSAMPLE;
}

uint32_t board_sdio_flags(void) {
    return SDIO_SLAVE_FLAG_DEFAULT_SPEED;
}

/* Jumper wires ring at the default drive strength and corrupt the data CRC. */
void board_sdio_pins_ready(void) {
    for (int gpio = GPIO_NUM_18; gpio <= GPIO_NUM_23; gpio++) {
        gpio_set_drive_capability(gpio, GPIO_DRIVE_CAP_0);
    }
}

#else

void board_init(void) {
}

sdio_slave_timing_t board_sdio_timing(void) {
    return SDIO_SLAVE_TIMING_PSEND_PSAMPLE;
}

uint32_t board_sdio_flags(void) {
    return SDIO_SLAVE_FLAG_HIGH_SPEED;
}

void board_sdio_pins_ready(void) {
}

#endif
