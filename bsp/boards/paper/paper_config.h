/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Paper (ESP32) board pin map. Shared across paper.c (power + SPI bus),
 * paper_panel.c (EPD + touch) and paper_sd.c (microSD) because the IT8951E EPD
 * and the microSD card sit on the same SPI bus, which paper.c owns. Pin numbers
 * come from the m5paper-bsp reference.
 */

#pragma once
#include "driver/gpio.h"
#include "driver/spi_common.h"
#include "driver/i2c_types.h"

/* Power-enable rails (sequenced in paper.c). */
#define PAPER_PIN_MAIN_PWR    GPIO_NUM_2
#define PAPER_PIN_EXT_PWR_EN  GPIO_NUM_5
#define PAPER_PIN_EPD_PWR_EN  GPIO_NUM_23

/* Shared SPI bus (IT8951E EPD + microSD). */
#define PAPER_SPI_HOST        SPI3_HOST
#define PAPER_SPI_PIN_SCK     GPIO_NUM_14
#define PAPER_SPI_PIN_MOSI    GPIO_NUM_12
#define PAPER_SPI_PIN_MISO    GPIO_NUM_13

/* IT8951E EPD controller. */
#define PAPER_EPD_PIN_CS      GPIO_NUM_15
#define PAPER_EPD_PIN_BUSY    GPIO_NUM_27
#define PAPER_VCOM_VOLTS      (-2.30f)

/* GT911 touch on its own I2C bus. INT is GPIO36 (input-only on ESP32, so the
 * driver attaches an ISR but never drives it); RESET is not exposed, so the
 * driver probes the I2C address. */
#define PAPER_I2C_PORT        I2C_NUM_1
#define PAPER_I2C_PIN_SDA     GPIO_NUM_21
#define PAPER_I2C_PIN_SCL     GPIO_NUM_22
#define PAPER_TOUCH_PIN_INT   GPIO_NUM_36
#define PAPER_TOUCH_PIN_RST   GPIO_NUM_NC

/* microSD chip-select on the shared SPI bus. */
#define PAPER_SD_PIN_CS       GPIO_NUM_4
