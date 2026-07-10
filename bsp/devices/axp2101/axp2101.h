/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AXP2101 PMIC driver: direct I2C register control of the DC-DC / LDO rails, the
 * battery-voltage ADC and the software shutdown path. create() only enables the
 * battery ADC; which rails to raise and to what voltage is left to explicit calls
 * by the caller. axp2101_power_create() wraps the handle as a bsp_power_t sensing
 * provider.
 *
 * The driver does NOT own the I2C bus: the caller initializes it and passes the
 * handle; the driver only attaches with i2c_master_bus_add_device().
 */

#pragma once
#include "bsp_types.h"
#include "bsp_power.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct axp2101_dev *axp2101_handle_t;

#define AXP2101_I2C_ADDR        0x34
#define AXP2101_I2C_DEFAULT_HZ  400000

/* Selectable output rails. DCDC1-5 are the buck converters; the rest are LDOs. */
typedef enum {
    AXP2101_DCDC1, AXP2101_DCDC2, AXP2101_DCDC3, AXP2101_DCDC4, AXP2101_DCDC5,
    AXP2101_ALDO1, AXP2101_ALDO2, AXP2101_ALDO3, AXP2101_ALDO4,
    AXP2101_BLDO1, AXP2101_BLDO2,
    AXP2101_DLDO1,
} axp2101_rail_t;

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t i2c_address;   /*!< 7-bit; 0 -> AXP2101_I2C_ADDR */
    int     clock_hz;      /*!< 0 -> AXP2101_I2C_DEFAULT_HZ */
} axp2101_config_t;

esp_err_t axp2101_create(const axp2101_config_t *cfg, axp2101_handle_t *out_handle);
esp_err_t axp2101_destroy(axp2101_handle_t h);

/* Raw register access (exposed for board-specific tweaks not covered below). */
esp_err_t axp2101_write_reg(axp2101_handle_t h, uint8_t reg, uint8_t val);
esp_err_t axp2101_read_reg(axp2101_handle_t h, uint8_t reg, uint8_t *val);

/* Set a rail's target voltage (mV). Clamped to the rail's range and quantized to
 * its step; setting the voltage does not enable the rail. ALDO/BLDO/DLDO are
 * 0.5-3.5 V, 100 mV step. */
esp_err_t axp2101_set_rail_mv(axp2101_handle_t h, axp2101_rail_t rail, int mv);

/* Enable/disable one rail. */
esp_err_t axp2101_set_rail_enabled(axp2101_handle_t h, axp2101_rail_t rail, bool on);

/* Battery terminal voltage (mV) from the built-in ADC. Returns 0 mV when no
 * battery is connected (the ADC input floats and reads garbage otherwise). */
esp_err_t axp2101_get_battery_voltage_mv(axp2101_handle_t h, uint32_t *out_mv);
bool      axp2101_battery_present(axp2101_handle_t h);
bool      axp2101_vbus_present(axp2101_handle_t h);

/* Software power-off: assert the shutdown bit. VBUS keeps the chip alive. */
esp_err_t axp2101_power_off(axp2101_handle_t h);

/* Wrap the handle as a bsp_power_t sensing provider. level is a linear map of the
 * battery voltage between empty_mv and full_mv. */
esp_err_t axp2101_power_create(axp2101_handle_t h, uint32_t empty_mv, uint32_t full_mv,
                               bsp_power_t **out_power);

#ifdef __cplusplus
}
#endif
