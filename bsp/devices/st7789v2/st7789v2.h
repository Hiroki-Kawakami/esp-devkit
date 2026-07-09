/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ST7789V2 SPI TFT driver, exposed as a bsp_display provider (draw_bitmap +
 * optional backlight + ON/SLEEP/OFF power). The controller GRAM is 240x320; a
 * smaller glass is addressed through the column/row offsets. Pixels are RGB565;
 * the glass wants them big-endian, so draw_bitmap byte-swaps the host-native
 * (little-endian) source while streaming it over queued DMA.
 *
 * The driver does NOT own the SPI bus: the caller initializes it (with a
 * max_transfer_sz of at least ST7789V2_DMA_CHUNK_BYTES) and passes the host;
 * the driver attaches with spi_bus_add_device() using hardware CS. The backlight
 * is not wired to the controller: set_brightness is routed to a board callback,
 * NULL when the board has no backlight control. The panel-IC rail is likewise a
 * board callback (set_panel_power), which set_power cuts for a full POWER_OFF.
 */

#pragma once
#include "bsp_types.h"
#include "bsp_display.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define ST7789V2_SPI_DEFAULT_HZ   (40 * 1000 * 1000)
#define ST7789V2_DMA_CHUNK_BYTES  4096

typedef void (*st7789v2_backlight_cb_t)(void *ctx, int brightness);
typedef void (*st7789v2_power_cb_t)(void *ctx, bool on);

typedef struct {
    spi_host_device_t spi_host;    /* host the bus was initialized on */
    gpio_num_t        cs_io;       /* CS (hardware-driven) */
    gpio_num_t        dc_io;       /* D/C, LOW = command, HIGH = data */
    gpio_num_t        reset_io;    /* RESET output, active LOW; NC to skip */
    int               clock_hz;    /* 0 -> ST7789V2_SPI_DEFAULT_HZ */

    uint16_t          width;       /* visible glass width (px) */
    uint16_t          height;      /* visible glass height (px) */
    uint16_t          x_offset;    /* column start into the 240-wide GRAM */
    uint16_t          y_offset;    /* row start into the 320-tall GRAM */
    uint8_t           madctl;      /* MADCTL (0x36) value: orientation + RGB/BGR */
    bool              invert;      /* INVON — IPS panels invert */

    /* Backlight control (NULL -> set_brightness is a no-op). */
    st7789v2_backlight_cb_t set_backlight;
    void                   *backlight_ctx;

    /* Panel-IC rail control (NULL -> POWER_OFF collapses to POWER_SLEEP). */
    st7789v2_power_cb_t     set_panel_power;
    void                   *panel_power_ctx;
} st7789v2_config_t;

esp_err_t st7789v2_create(const st7789v2_config_t *cfg, bsp_display_t **out_display);

#ifdef __cplusplus
}
#endif
