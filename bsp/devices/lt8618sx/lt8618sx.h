/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * LT8618SX RGB-to-HDMI transmitter, controlled over I2C. Registers are paged:
 * writing 0xFF selects the page the following register addresses belong to, so
 * every sequence here starts by selecting one. Not a bsp_* provider — the video
 * source (a panel driver feeding the transmitter's RGB input) owns it and calls
 * init/reset around its own power states.
 *
 * The register sequences are the ones LovyanGFX's Panel_M5HDMI HDMI_Trans class
 * uses; the datasheet does not document them.
 */

#pragma once
#include "bsp_types.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define LT8618SX_I2C_ADDRESS   0x39
#define LT8618SX_I2C_CLOCK_HZ  100000

typedef struct lt8618sx_state *lt8618sx_t;

esp_err_t lt8618sx_create(i2c_master_bus_handle_t i2c_bus, uint8_t address, lt8618sx_t *out);
esp_err_t lt8618sx_delete(lt8618sx_t dev);

/* Three ID bytes. Three identical bytes mean nothing answered on the bus (the
 * caller reads back its own idle level), i.e. no transmitter present. */
esp_err_t lt8618sx_read_chip_id(lt8618sx_t dev, uint8_t out_id[3]);

/* Stops the output and returns the chip to its power-on register state. Also
 * the sleep entry: init() brings it back. */
esp_err_t lt8618sx_reset(lt8618sx_t dev);

/* Full bring-up: audio (48 kHz I2S), then the TX PLL, retried until it reports
 * locked. ESP_ERR_INVALID_STATE when the PLL never locks. */
esp_err_t lt8618sx_init(lt8618sx_t dev);

#ifdef __cplusplus
}
#endif
