/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Gowin JTAG SRAM loader for the video FPGA, used when the module answers no
 * gateware ID over SPI. The four pins are the module port's SPI pins in their
 * JTAG roles; the caller must have detached them from the SPI peripheral first
 * (this code drives them as plain GPIOs) and re-attaches them afterwards.
 *
 * Without CONFIG_BSP_M5_HDMI_FPGA_BITSTREAM the bitstream is not linked in and
 * this returns ESP_ERR_NOT_SUPPORTED.
 */

#pragma once
#include "bsp_types.h"
#include "driver/gpio.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t m5_hdmi_fpga_jtag_load(gpio_num_t tck_io, gpio_num_t tdi_io,
                                 gpio_num_t tdo_io, gpio_num_t tms_io);

#ifdef __cplusplus
}
#endif
