/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GDEY0154D67 1.54" 200x200 B/W e-paper (SSD1681 controller), SPI mode.
 * The driver uploads GoodDisplay's waveform LUTs (via 0x32) rather than the
 * SSD1681 OTP waveform (fewer flashes): update_full drives the flashing full
 * waveform, update_partial the direct-update (no-flash) waveform. Both take a
 * whole-frame 1bpp buffer; there is no windowed area update. The caller decides
 * the full/partial cadence (e.g. a periodic full to clear ghosting).
 *
 * The driver does NOT own the SPI bus: the caller initializes it via
 * spi_bus_initialize() and passes the host; the driver only attaches with
 * spi_bus_add_device(). CS is driven manually (spics_io_num = -1) so a whole
 * command/data packet stays framed while DC toggles.
 */

#pragma once

#include <stdint.h>
#include "esp_err.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct gdey0154d67_dev *gdey0154d67_handle_t;

#define GDEY0154D67_SPI_MODE        0
#define GDEY0154D67_SPI_DEFAULT_HZ  (20 * 1000 * 1000)
#define GDEY0154D67_DEFAULT_WIDTH   200
#define GDEY0154D67_DEFAULT_HEIGHT  200

typedef struct {
    spi_host_device_t spi_host;   /* host the bus was initialized on        */
    gpio_num_t        cs_io;      /* CS, manual (active low)                */
    gpio_num_t        dc_io;      /* D/C, LOW = command, HIGH = data        */
    gpio_num_t        reset_io;   /* RESET output, active LOW; NC to skip   */
    gpio_num_t        busy_io;    /* BUSY input, HIGH = busy                */
    int               clock_hz;   /* 0 -> GDEY0154D67_SPI_DEFAULT_HZ        */
    uint16_t          width;      /* 0 -> GDEY0154D67_DEFAULT_WIDTH         */
    uint16_t          height;     /* 0 -> GDEY0154D67_DEFAULT_HEIGHT        */
} gdey0154d67_config_t;

esp_err_t gdey0154d67_create(const gdey0154d67_config_t *cfg, gdey0154d67_handle_t *out_handle);
esp_err_t gdey0154d67_destroy(gdey0154d67_handle_t handle);

/* Hardware reset + re-run the init sequence. */
esp_err_t gdey0154d67_reset(gdey0154d67_handle_t handle);

/* Push a full-frame 1bpp image (packed MSB-first, bit 1 = white) and drive it.
 * `packed` is width/8 * height bytes; both block until the refresh completes.
 * update_full: flashing full waveform, re-seeds the partial base.
 * update_partial: direct-update waveform (diffs against the previous frame);
 * needs a prior update_full to establish the baseline. */
esp_err_t gdey0154d67_update_full(gdey0154d67_handle_t handle, const uint8_t *packed);
esp_err_t gdey0154d67_update_partial(gdey0154d67_handle_t handle, const uint8_t *packed);

/* Deep sleep (lowest power; a create/reset is needed to wake). */
esp_err_t gdey0154d67_sleep(gdey0154d67_handle_t handle);

/* Block until BUSY is deasserted. timeout_ms = 0 -> wait forever. */
esp_err_t gdey0154d67_wait_idle(gdey0154d67_handle_t handle, uint32_t timeout_ms);

#ifdef __cplusplus
}
#endif
