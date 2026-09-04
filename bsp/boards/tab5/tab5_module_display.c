/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5Stack Tab5 side of the HDMI video module: the M-Bus pin map and the SPI
 * host the module gets, plus the I2C bus its transmitter shares with the rest
 * of the board. The module driver owns the SPI bus (it has to bit-bang those
 * pins to load the FPGA), so nothing else on the board may use SPI2.
 */

#include "tab5_module_display.h"
#include "esp_log.h"
#include "driver/i2c_master.h"
#include "m5_hdmi_fpga.h"

static const char *TAG = "tab5";

#define TAB5_MBUS_SPI_HOST  SPI2_HOST
#define TAB5_MBUS_PIN_SCLK  GPIO_NUM_5
#define TAB5_MBUS_PIN_MOSI  GPIO_NUM_18
#define TAB5_MBUS_PIN_MISO  GPIO_NUM_19
#define TAB5_MBUS_PIN_CS    GPIO_NUM_48

esp_err_t tab5_module_display_attach(const bsp_display_module_config_t *config,
                                     bsp_panel_id_t *out_panel) {
    if (config && config->pixel_format != BSP_PIXEL_FORMAT_RGB565 &&
        config->pixel_format != 0) {
        return ESP_ERR_NOT_SUPPORTED;
    }

    i2c_master_bus_handle_t i2c_bus = bsp_bus_get_i2c_handle(I2C_NUM_0);
    if (!i2c_bus) return ESP_ERR_INVALID_STATE;

    const m5_hdmi_fpga_config_t module_config = {
        .spi_host = TAB5_MBUS_SPI_HOST,
        .sclk_io  = TAB5_MBUS_PIN_SCLK,
        .mosi_io  = TAB5_MBUS_PIN_MOSI,
        .miso_io  = TAB5_MBUS_PIN_MISO,
        .cs_io    = TAB5_MBUS_PIN_CS,
        .i2c_bus  = i2c_bus,
        .size     = config ? config->size : (bsp_size_t){0, 0},
    };
    bsp_display_t *display = NULL;
    esp_err_t err = m5_hdmi_fpga_create(&module_config, &display);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "display module on M-Bus unavailable: %s", esp_err_to_name(err));
        return err;
    }
    return bsp_display_attach(display, out_panel);
}
