/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ILI9342C SPI TFT driver, exposed as a bsp_display provider (draw_bitmap +
 * optional backlight + ON/SLEEP/OFF power). The controller drives a 320x240
 * glass directly (no column/row offset). Pixels are RGB565; the glass wants them
 * big-endian, so draw_bitmap byte-swaps the host-native (little-endian) source
 * while streaming it over queued DMA.
 *
 * The driver does NOT own the SPI bus: the caller initializes it (with a
 * max_transfer_sz of at least ILI9342C_DMA_CHUNK_BYTES) and passes the host; the
 * driver attaches with spi_bus_add_device() using hardware CS. RESET is a board
 * callback (set_reset) because on some boards it hangs off an I/O expander rather
 * than a native GPIO; NULL falls back to the software reset command. The backlight
 * is likewise a board callback (set_backlight), NULL when the board has no
 * control, and the panel-IC rail is set_panel_power (NULL -> OFF collapses to
 * SLEEP).
 */

#pragma once
#include "bsp_types.h"
#include "bsp_display.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define ILI9342C_SPI_DEFAULT_HZ   (40 * 1000 * 1000)
#define ILI9342C_DMA_CHUNK_BYTES  4096

typedef void (*ili9342c_reset_cb_t)(void *ctx, bool asserted);
typedef void (*ili9342c_backlight_cb_t)(void *ctx, int brightness);
typedef void (*ili9342c_power_cb_t)(void *ctx, bool on);

typedef struct {
    spi_host_device_t spi_host;    /* host the bus was initialized on */
    gpio_num_t        cs_io;       /* CS (hardware-driven) */
    gpio_num_t        dc_io;       /* D/C, LOW = command, HIGH = data */
    int               clock_hz;    /* 0 -> ILI9342C_SPI_DEFAULT_HZ */

    uint16_t          width;       /* visible glass width (px), 320 landscape */
    uint16_t          height;      /* visible glass height (px), 240 landscape */
    uint8_t           madctl;      /* MADCTL (0x36): orientation + RGB/BGR */
    bool              invert;      /* INVON — IPS panels invert */

    /* Hardware RESET line (NULL -> software reset only). asserted==true drives
     * RESET active (LOW on the pin behind the callback). */
    ili9342c_reset_cb_t     set_reset;
    void                   *reset_ctx;

    /* Backlight control (NULL -> set_brightness is a no-op). */
    ili9342c_backlight_cb_t set_backlight;
    void                   *backlight_ctx;

    /* Panel-IC rail control (NULL -> POWER_OFF collapses to POWER_SLEEP). */
    ili9342c_power_cb_t     set_panel_power;
    void                   *panel_power_ctx;
} ili9342c_config_t;

esp_err_t ili9342c_create(const ili9342c_config_t *cfg, bsp_display_t **out_display);

#ifdef __cplusplus
}
#endif
