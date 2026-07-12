/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator-only SCD40 chip emulator: speaks the Sensirion I2C protocol on the
 * idf_compat virtual bus so the real scd40.c driver runs unmodified on host.
 * Board *_sim.c bring-up attaches it (the sim counterpart of the soldered
 * chip). Reproduces the mode state machine, the 5 s / 30 s data-ready cadence
 * (read clears it, reading while not ready NACKs), idle-only command
 * enforcement, and the stored ASC / offset / altitude settings. Values default
 * to a plausible slow random walk.
 */

#pragma once
#include "scd40.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct scd40_sim scd40_sim_t;

/* address 0 -> SCD40_I2C_ADDR; out_chip may be NULL when the caller never
 * detaches or injects values. */
esp_err_t scd40_sim_attach(i2c_master_bus_handle_t bus, uint8_t address, scd40_sim_t **out_chip);
void      scd40_sim_detach(scd40_sim_t *chip);

/* Pin the reported values; NULL resumes the automatic random walk. */
void scd40_sim_set_environment(scd40_sim_t *chip, const scd40_data_t *data);

#ifdef __cplusplus
}
#endif
