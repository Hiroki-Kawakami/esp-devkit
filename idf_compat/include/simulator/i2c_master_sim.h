/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator-only registry of virtual I2C peripherals for the i2c_master host
 * shim. A chip emulator attaches transfer callbacks at a 7-bit address on a
 * bus created with i2c_new_master_bus(); board sim bring-up does the attaching
 * (the sim counterpart of soldering the chip to the board). Callbacks run on
 * the caller's task with the bus locked — return ESP_OK to ACK, anything else
 * to NACK the transfer.
 */

#pragma once
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    esp_err_t (*on_write)(void *ctx, const uint8_t *data, size_t len);
    esp_err_t (*on_read)(void *ctx, uint8_t *data, size_t len);
} i2c_sim_device_ops_t;

esp_err_t i2c_sim_attach(i2c_master_bus_handle_t bus, uint16_t address,
                         const i2c_sim_device_ops_t *ops, void *ctx);
esp_err_t i2c_sim_detach(i2c_master_bus_handle_t bus, uint16_t address);

#ifdef __cplusplus
}
#endif
