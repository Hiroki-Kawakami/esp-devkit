/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator-only SEN55 chip emulator: speaks the Sensirion I2C protocol on the
 * idf_compat virtual bus so the real sen55.c driver runs unmodified on host.
 * Board *_sim.c bring-up attaches it (the sim counterpart of the soldered
 * chip). Reproduces the mode state machine, the 1 s data-ready cadence
 * (read clears it), warm-up NAN for VOC/NOx, and NACKs for commands invalid
 * in the current mode. Values default to a plausible slow random walk.
 */

#pragma once
#include "sen55.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct sen55_sim sen55_sim_t;

/* address 0 -> SEN55_I2C_ADDR; out_chip may be NULL when the caller never
 * detaches or injects values. */
esp_err_t sen55_sim_attach(i2c_master_bus_handle_t bus, uint8_t address, sen55_sim_t **out_chip);
void      sen55_sim_detach(sen55_sim_t *chip);

/* Pin the reported environment (NAN fields report as chip-invalid); NULL
 * resumes the automatic random walk. */
void sen55_sim_set_environment(sen55_sim_t *chip, const sen55_data_t *data);

#ifdef __cplusplus
}
#endif
