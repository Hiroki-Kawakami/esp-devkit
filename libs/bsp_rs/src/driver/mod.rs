/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(CONFIG_BSP_RS_DRIVER_ADC_BATTERY)]
pub(crate) mod adc_battery;
#[cfg(any(test, CONFIG_BSP_RS_DRIVER_EPD_LL))]
pub(crate) mod epd_ll;
#[cfg(CONFIG_BSP_RS_DRIVER_ESP_HOSTED)]
pub(crate) mod esp_hosted;
#[cfg(CONFIG_BSP_RS_DRIVER_GPIO_BUTTON)]
pub(crate) mod gpio_button;
#[cfg(CONFIG_BSP_RS_DRIVER_I2S_OUT)]
pub(crate) mod i2s_out;
#[cfg(CONFIG_BSP_RS_DRIVER_MIPI_DSI)]
pub(crate) mod mipi_dsi;
#[cfg(CONFIG_BSP_RS_DRIVER_SDMMC)]
pub(crate) mod sdmmc;
#[cfg(CONFIG_BSP_RS_DRIVER_SPI_LCD)]
pub(crate) mod spi_lcd;
