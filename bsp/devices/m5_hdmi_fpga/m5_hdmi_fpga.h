/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * HDMI video module built around a Gowin GW1NR-9 FPGA and an LT8618SX
 * transmitter, exposed as a bsp_display provider. The FPGA holds the frame
 * buffer and generates the video timing; the host only pushes rectangles of
 * pixels over SPI, so this provider offers draw_bitmap (no host framebuffer,
 * no backlight) and maps panel power onto the transmitter's output.
 *
 * The command protocol is the M5Stack gateware's, not the FPGA vendor's: a
 * one-byte command optionally followed by big-endian parameters, with a status
 * byte read back on MISO to tell when the FPGA has drained the last write.
 *
 * Unlike the panel drivers, this one owns the SPI bus (the caller passes the
 * host and the pins, uninitialized): the gateware the FPGA runs is held in SRAM
 * and is lost whenever the host resets, so create() bit-bangs it back in over
 * the same four pins as JTAG (SCLK/MOSI/MISO/CS as TCK/TDI/TDO/TMS), which means
 * the bus has to be released and re-initialized mid-bring-up. Budget ~4 s for a
 * bring-up that has to reload. Nothing else on the board may share that host. CS
 * is a plain GPIO here because the status handshake needs it toggled between
 * transfers.
 *
 * Bring-up leaves the frame buffer as it found it (undefined after a reload) —
 * the caller draws, or asks for a known baseline with bsp_display_clear().
 */

#pragma once
#include "bsp_types.h"
#include "bsp_display.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

/* The gateware accepts 80 MHz, but a module stacked on a board's expansion
 * connector does not reliably carry it: corrupt bytes desynchronize the pixel
 * stream and leave parts of the written rectangle stale. 40 MHz measured clean;
 * a board with better wiring can raise it through the config. */
#define M5_HDMI_FPGA_WRITE_CLOCK_HZ  (40 * 1000 * 1000)
#define M5_HDMI_FPGA_READ_CLOCK_HZ   (16 * 1000 * 1000)
#define M5_HDMI_FPGA_PIXEL_CLOCK_HZ  74250000
#define M5_HDMI_FPGA_DMA_CHUNK_BYTES 4096

typedef struct {
    spi_host_device_t spi_host;      /*!< bus the caller initialized */
    gpio_num_t        sclk_io;
    gpio_num_t        mosi_io;
    gpio_num_t        miso_io;
    gpio_num_t        cs_io;
    int               write_clock_hz; /*!< 0 -> M5_HDMI_FPGA_WRITE_CLOCK_HZ */
    int               read_clock_hz;  /*!< 0 -> M5_HDMI_FPGA_READ_CLOCK_HZ */

    i2c_master_bus_handle_t i2c_bus;  /*!< bus the LT8618SX sits on */
    uint8_t                 i2c_address; /*!< 0 -> LT8618SX_I2C_ADDRESS */

    bsp_size_t size;          /*!< output resolution; {0,0} -> 1280x720 */
    float      refresh_rate;  /*!< Hz; 0 -> 60, or 30/24 for large modes */
    uint32_t   pixel_clock;   /*!< 0 -> M5_HDMI_FPGA_PIXEL_CLOCK_HZ */
} m5_hdmi_fpga_config_t;

BSP_NONNULL(1, 2) esp_err_t m5_hdmi_fpga_create(const m5_hdmi_fpga_config_t *config,
                                                 bsp_display_t **out);

#ifdef __cplusplus
}
#endif
