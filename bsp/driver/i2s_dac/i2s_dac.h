/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Generic I2S DAC / amplifier output (no I2C control): a driver/i2s_std TX
 * channel. Two sinks share the same transport and software gain:
 *
 *   - External 16-bit I2S DAC/amp on bclk/ws/dout (e.g. NS4168 / MAX98357).
 *   - ESP32 built-in DAC, by routing the I2S0 DMA to the DAC pins (internal_dac).
 *
 * Both feed a standard I2S channel with auto_clear, so an underrun just clocks
 * out silence instead of stalling — unlike the dac_continuous acyclic path.
 * Volume and mute are a software gain applied to the samples in write(), so
 * they hold regardless of the bsp_audio DSP stage. Analog amp enable (if any)
 * is a board concern and lives outside this driver.
 */

#pragma once
#include "bsp_types.h"
#include "driver/gpio.h"
#include "driver/i2s_types.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Set as i2s_port to select the ESP32 built-in DAC instead of an external I2S
 * sink (routes the I2S0 DMA to the DAC). Chosen to not collide with the real
 * i2s_port_t ids (I2S_NUM_0/1, I2S_NUM_AUTO = -1). */
#define I2S_NUM_INTERNAL  (-2)

typedef struct i2s_dac_state *i2s_dac_t;

typedef struct {
    int i2s_port;   /*!< I2S controller id, or I2S_NUM_INTERNAL for the built-in DAC */
    union {
        struct {                  /*!< external I2S sink */
            gpio_num_t bclk_gpio;
            gpio_num_t ws_gpio;   /*!< LRCK */
            gpio_num_t dout_gpio; /*!< DATA; MCLK/DIN unused (TX only) */
        };
        gpio_num_t dac_gpio;      /*!< I2S_NUM_INTERNAL: GPIO25 (DAC ch0) or GPIO26 (DAC ch1) */
    };
} i2s_dac_config_t;

esp_err_t i2s_dac_create(const i2s_dac_config_t *config, i2s_dac_t *out);
esp_err_t i2s_dac_deinit(i2s_dac_t s);

esp_err_t i2s_dac_open(i2s_dac_t s, uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels);
esp_err_t i2s_dac_close(i2s_dac_t s);
esp_err_t i2s_dac_write(i2s_dac_t s, const void *data, size_t len);

esp_err_t i2s_dac_set_volume(i2s_dac_t s, int volume);  /*!< 0..100 */
esp_err_t i2s_dac_set_mute(i2s_dac_t s, bool mute);

#ifdef __cplusplus
}
#endif
