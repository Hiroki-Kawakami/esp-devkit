/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AXP192 PMIC driver: direct I2C register control of the DC-DC / LDO rails, the
 * built-in ADCs (battery + VBUS) and the shutdown path. The driver only sets
 * ADC/charge defaults in create(); which rails to raise and to what voltage is
 * left to explicit calls by the caller. axp192_power_create() wraps the handle
 * as a bsp_power_t sensing provider.
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

typedef struct axp192_dev *axp192_handle_t;

#define AXP192_I2C_ADDR        0x34
#define AXP192_I2C_DEFAULT_HZ  400000

/* Rail bits — laid out to match the AXP192 output-enable register (0x12) so the
 * mask doubles as its bitfield. */
#define AXP192_RAIL_DCDC3  (1u << 0)
#define AXP192_RAIL_DCDC1  (1u << 1)
#define AXP192_RAIL_LDO2   (1u << 2)
#define AXP192_RAIL_LDO3   (1u << 3)
#define AXP192_RAIL_DCDC2  (1u << 4)
#define AXP192_RAIL_EXTEN  (1u << 6)

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t i2c_address;   /*!< 7-bit; 0 -> AXP192_I2C_ADDR */
    int     clock_hz;      /*!< 0 -> AXP192_I2C_DEFAULT_HZ */
} axp192_config_t;

esp_err_t axp192_create(const axp192_config_t *cfg, axp192_handle_t *out_handle);
esp_err_t axp192_destroy(axp192_handle_t handle);

/* Raw register access (exposed for board-specific tweaks not covered below). */
esp_err_t axp192_write_reg(axp192_handle_t h, uint8_t reg, uint8_t val);
esp_err_t axp192_read_reg(axp192_handle_t h, uint8_t reg, uint8_t *val);

/* Rail voltage setters. Values are clamped to the chip's range and quantized to
 * its step; setting the voltage does not enable the rail (use set_rail_enabled).
 * LDO2/LDO3: 1.8-3.3 V, 100 mV step. DCDC1/DCDC3: 0.7-3.5 V, 25 mV step.
 * DCDC2: 0.7-2.275 V, 25 mV step. LDO1 is a fixed 3.3 V always-on RTC rail (not
 * adjustable), so it has no setter. */
esp_err_t axp192_set_ldo2_mv(axp192_handle_t h, int mv);
esp_err_t axp192_set_ldo3_mv(axp192_handle_t h, int mv);
esp_err_t axp192_set_dcdc1_mv(axp192_handle_t h, int mv);
esp_err_t axp192_set_dcdc2_mv(axp192_handle_t h, int mv);
esp_err_t axp192_set_dcdc3_mv(axp192_handle_t h, int mv);

/* Enable/disable one or more rails (OR of AXP192_RAIL_*). */
esp_err_t axp192_set_rail_enabled(axp192_handle_t h, uint32_t rail_mask, bool on);

/* GPIO0 driven as a low-noise LDO (LDOio0): 1.8-3.3 V, 100 mV step. Enabling
 * switches GPIO0 into LDO mode; disabling floats it. Sits in its own registers,
 * so it is not part of the AXP192_RAIL_* mask. */
esp_err_t axp192_set_ldoio0_mv(axp192_handle_t h, int mv);
esp_err_t axp192_set_ldoio0_enabled(axp192_handle_t h, bool on);

/* Sensing. voltage returns the battery terminal voltage in mV; vbus_present is
 * the VBUS (USB) presence bit. */
esp_err_t axp192_get_battery_voltage_mv(axp192_handle_t h, uint32_t *out_mv);
bool      axp192_vbus_present(axp192_handle_t h);

/* Software power-off: cut every rail via the shutdown bit. External power on
 * VBUS keeps the chip alive, so this may not actually kill VSYS. */
esp_err_t axp192_power_off(axp192_handle_t h);

/* Wrap the handle as a bsp_power_t sensing provider. level is a linear map of
 * the battery voltage between empty_mv and full_mv. */
esp_err_t axp192_power_create(axp192_handle_t h, uint32_t empty_mv, uint32_t full_mv,
                              bsp_power_t **out_power);

#ifdef __cplusplus
}
#endif
