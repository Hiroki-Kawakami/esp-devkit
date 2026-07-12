/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * SCD40 CO2 sensor I2C driver. The caller passes an initialized I2C bus
 * handle; the driver attaches as a device on it and does not own the bus.
 * create() probes the address, so a missing sensor fails there. One handle per
 * task; some calls block for the chip's command execution time (stop ~500 ms,
 * forced recalibration ~400 ms, persist ~800 ms). On the simulator the same
 * driver talks to the emulated chip (scd40_sim.h) on the idf_compat virtual
 * bus.
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include "esp_err.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define SCD40_I2C_ADDR        0x62
#define SCD40_I2C_DEFAULT_HZ  (100 * 1000)

typedef struct {
    i2c_master_bus_handle_t i2c_bus;      /*!< initialized bus, not owned */
    uint8_t                 i2c_address;  /*!< 0 -> SCD40_I2C_ADDR */
    uint32_t                clock_hz;     /*!< 0 -> SCD40_I2C_DEFAULT_HZ */
} scd40_config_t;

typedef struct scd40 scd40_t;

typedef struct {
    uint16_t co2_ppm;
    float    temperature;  /*!< degC */
    float    humidity;     /*!< %RH */
} scd40_data_t;

esp_err_t scd40_create(const scd40_config_t *config, scd40_t **out);
void      scd40_delete(scd40_t *sensor);

/* start is only accepted from idle (stop first after a soft reboot — the chip
 * keeps measuring across host resets). Periods: 5 s, low_power 30 s. */
esp_err_t scd40_start(scd40_t *sensor);
esp_err_t scd40_start_low_power(scd40_t *sensor);
esp_err_t scd40_stop(scd40_t *sensor);

/* read is only valid while ready is set (the chip NACKs otherwise) and
 * clears the ready flag. */
esp_err_t scd40_data_ready(scd40_t *sensor, bool *out_ready);
esp_err_t scd40_read(scd40_t *sensor, scd40_data_t *out);

/* Calibration / compensation. Except set_ambient_pressure these are only
 * accepted while idle; they take effect in RAM — persist_settings writes them
 * to chip EEPROM (limited write cycles, call sparingly). */
esp_err_t scd40_set_asc_enabled(scd40_t *sensor, bool enabled);
esp_err_t scd40_get_asc_enabled(scd40_t *sensor, bool *out_enabled);
esp_err_t scd40_forced_recalibration(scd40_t *sensor, uint16_t target_ppm, int16_t *out_correction);
esp_err_t scd40_set_temperature_offset(scd40_t *sensor, float offset_degc);
esp_err_t scd40_set_altitude(scd40_t *sensor, uint16_t meters);
esp_err_t scd40_set_ambient_pressure(scd40_t *sensor, uint32_t pascals);
esp_err_t scd40_persist_settings(scd40_t *sensor);
esp_err_t scd40_read_serial(scd40_t *sensor, uint64_t *out_serial);
esp_err_t scd40_reset(scd40_t *sensor);  /* reinit from EEPROM, not a factory reset */

#ifdef __cplusplus
}
#endif
