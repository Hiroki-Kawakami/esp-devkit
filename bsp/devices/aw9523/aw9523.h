/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AW9523B 16-bit I2C GPIO/LED expander (two 8-bit ports P0/P1). Used by boards
 * that route control signals (LCD/touch reset, amp enable, rail enables) through
 * an I/O expander instead of native GPIO. Not a bsp_* provider on its own —
 * boards consume it directly during bring-up.
 *
 * Pins are addressed 0..15: 0..7 = P0_0..P0_7, 8..15 = P1_0..P1_7. Every pin is
 * put in GPIO (not constant-current LED) mode on init; P0 outputs are push-pull.
 * The driver does NOT own the I2C bus: the caller passes the handle and the
 * driver attaches with i2c_master_bus_add_device().
 */

#pragma once
#include "bsp_types.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    AW9523_PIN_MODE_NONE,
    AW9523_PIN_MODE_INPUT,
    AW9523_PIN_MODE_OUTPUT,
} aw9523_pin_mode_t;

typedef struct {
    aw9523_pin_mode_t mode : 2;
    bool              initial_value : 1;   /*!< output pins: level driven at init */
} aw9523_pin_config_t;

typedef struct aw9523_state *aw9523_t;

#define AW9523_I2C_DEFAULT_HZ  400000

esp_err_t aw9523_init(i2c_master_bus_handle_t i2c_bus, uint8_t address,
                      const aw9523_pin_config_t config[16], aw9523_t *out);
esp_err_t aw9523_deinit(aw9523_t aw);

/* pin: 0..15 (P0_0..P0_7, then P1_0..P1_7). */
esp_err_t aw9523_set_output(aw9523_t aw, uint8_t pin, bool value);
esp_err_t aw9523_get_input(aw9523_t aw, uint8_t pin, bool *value);

#ifdef __cplusplus
}
#endif
