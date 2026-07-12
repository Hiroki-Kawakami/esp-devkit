/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * SEN55 air quality sensor (PM / RH / T / VOC / NOx) I2C driver. The caller
 * passes an initialized I2C bus handle; the driver attaches as a device on it
 * and does not own the bus. create() probes the address, so a missing (or
 * still powering up, see SEN55_POWER_UP_MS) sensor fails there. One handle per
 * task; calls block for the chip's command execution time (tens of ms). On the
 * simulator the same driver talks to the emulated chip (sen55_sim.h) on the
 * idf_compat virtual bus.
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

#define SEN55_I2C_ADDR        0x69
#define SEN55_I2C_DEFAULT_HZ  (100 * 1000)  /* chip max */
#define SEN55_POWER_UP_MS     100           /* rail-on until the chip is addressable */

typedef struct {
    i2c_master_bus_handle_t i2c_bus;      /*!< initialized bus, not owned */
    uint8_t                 i2c_address;  /*!< 0 -> SEN55_I2C_ADDR */
    uint32_t                clock_hz;     /*!< 0 -> SEN55_I2C_DEFAULT_HZ */
} sen55_config_t;

typedef struct sen55 sen55_t;

/* Values the chip has not produced yet (warm-up, or PM in RHT/gas-only mode)
 * come back as NAN. voc/nox are unitless 1..500 indices. */
typedef struct {
    float pm1_0;        /*!< ug/m3 */
    float pm2_5;        /*!< ug/m3 */
    float pm4_0;        /*!< ug/m3 */
    float pm10_0;       /*!< ug/m3 */
    float humidity;     /*!< %RH */
    float temperature;  /*!< degC */
    float voc_index;
    float nox_index;
} sen55_data_t;

esp_err_t sen55_create(const sen55_config_t *config, sen55_t **out);
void      sen55_delete(sen55_t *sensor);

/* Mode changes: start* is only accepted from idle (stop first after a soft
 * reboot — the chip keeps measuring across host resets). rht_gas_only keeps
 * the fan off and reports PM as NAN. */
esp_err_t sen55_start(sen55_t *sensor);
esp_err_t sen55_start_rht_gas_only(sen55_t *sensor);
esp_err_t sen55_stop(sen55_t *sensor);

/* New values arrive every 1 s while measuring; read clears the ready flag. */
esp_err_t sen55_data_ready(sen55_t *sensor, bool *out_ready);
esp_err_t sen55_read(sen55_t *sensor, sen55_data_t *out);

esp_err_t sen55_fan_clean(sen55_t *sensor);  /* manual fan cleaning (~10 s), measuring only */
esp_err_t sen55_read_status(sen55_t *sensor, uint32_t *out_flags);  /* chip device-status register */
esp_err_t sen55_read_serial(sen55_t *sensor, char *out, size_t len);
esp_err_t sen55_reset(sen55_t *sensor);

#ifdef __cplusplus
}
#endif
