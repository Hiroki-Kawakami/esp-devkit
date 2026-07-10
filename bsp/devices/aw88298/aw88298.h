/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AW88298 mono Class-D amplifier driver (I2S input, no ADC/RX): direct I2C
 * register control plus a driver/i2s_std TX channel. init() leaves the amp
 * disabled and muted — silent until open(). Volume is the chip's digital VOL
 * (reg 0x0C), mute is AMPPD (reg 0x04); the analog amp enable pin is a board
 * concern and lives outside this driver.
 */

#pragma once
#include "bsp_types.h"
#include "driver/gpio.h"
#include "driver/i2s_types.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct aw88298_state *aw88298_t;

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t i2c_address;       /*!< 7-bit; 0 -> AW88298 default 0x36 */
    int i2s_port;              /*!< I2S controller id, e.g. I2S_NUM_1 */
    gpio_num_t bclk_gpio;
    gpio_num_t ws_gpio;
    gpio_num_t dout_gpio;      /*!< MCLK/DIN are unused (BCLK-derived, TX only) */
} aw88298_config_t;

esp_err_t aw88298_init(const aw88298_config_t *config, aw88298_t *aw88298);
esp_err_t aw88298_deinit(aw88298_t aw88298);

esp_err_t aw88298_open(aw88298_t aw88298, uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels);
esp_err_t aw88298_close(aw88298_t aw88298);
esp_err_t aw88298_write(aw88298_t aw88298, const void *data, size_t len);
esp_err_t aw88298_set_volume(aw88298_t aw88298, int volume);  /*!< 0..100 */
esp_err_t aw88298_set_mute(aw88298_t aw88298, bool mute);     /*!< AMPPD */
int       aw88298_get_volume(aw88298_t aw88298);

#ifdef __cplusplus
}
#endif
