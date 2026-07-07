/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ES8388 DAC-output driver: direct I2C register control plus a driver/i2s_std
 * TX channel. init() leaves the DAC powered down and muted — silent until
 * open(). Output routing is board-selectable via `dac_outputs`.
 */

#pragma once
#include "bsp_types.h"
#include "driver/gpio.h"
#include "driver/i2s_types.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

/* DACPOWER (reg 0x04) output-enable bits — OR together into dac_outputs. */
#define ES8388_OUT_LOUT1 0x04
#define ES8388_OUT_LOUT2 0x08
#define ES8388_OUT_ROUT1 0x10
#define ES8388_OUT_ROUT2 0x20

typedef struct es8388_state *es8388_t;

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t i2c_address;       /*!< 7-bit; 0 -> ES8388 default 0x10 */
    int i2s_port;              /*!< I2S controller id, e.g. I2S_NUM_0 */
    gpio_num_t mclk_gpio;
    gpio_num_t bclk_gpio;
    gpio_num_t ws_gpio;
    gpio_num_t dout_gpio;
    gpio_num_t din_gpio;       /*!< GPIO_NUM_NC to disable RX */
    uint8_t dac_outputs;       /*!< ES8388_OUT_* mask; 0 -> LOUT1 | ROUT1 */
} es8388_config_t;

esp_err_t es8388_init(const es8388_config_t *config, es8388_t *es8388);
esp_err_t es8388_deinit(es8388_t es8388);

esp_err_t es8388_open(es8388_t es8388, uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels);
esp_err_t es8388_close(es8388_t es8388);
esp_err_t es8388_write(es8388_t es8388, const void *data, size_t len);
esp_err_t es8388_set_volume(es8388_t es8388, int volume);  /*!< 0..100, 0 mutes */
esp_err_t es8388_set_mute(es8388_t es8388, bool mute);
int       es8388_get_volume(es8388_t es8388);

#ifdef __cplusplus
}
#endif
