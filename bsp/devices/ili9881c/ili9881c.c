/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ili9881c.h"
#include "ili9881c_init_data.h"
#include "mipi_dsi.h"
#include "esp_lcd_panel_commands.h"

static esp_err_t send_init_sequence(esp_lcd_panel_io_handle_t io,
                                    bsp_pixel_format_t pixel_format,
                                    void *user_data) {
    (void)user_data;

    const uint8_t madctl = 0x00;
    const uint8_t colmod = pixel_format == BSP_PIXEL_FORMAT_RGB888 ? 0x77 : 0x55;
    const mipi_dsi_init_cmd_t prefix[] = {
        { LCD_CMD_SWRESET, NULL, 0, 20 },
        { LCD_CMD_SLPOUT,  NULL, 0, 120 },
        { LCD_CMD_MADCTL,  &madctl, 1, 0 },
        { LCD_CMD_COLMOD,  &colmod, 1, 0 },
    };

    esp_err_t err = mipi_dsi_send_init_cmds(io, prefix, sizeof(prefix) / sizeof(prefix[0]));
    if (err != ESP_OK) return err;
    return mipi_dsi_send_init_cmds(io, ili9881c_init_cmds, ili9881c_init_cmds_count);
}

esp_err_t ili9881c_lcd_create(const ili9881c_config_t *config, bsp_display_t **out) {
    const mipi_dsi_config_t mipi_config = {
        .size               = config->size,
        .pixel_format       = config->pixel_format,
        .fb_num             = config->fb_num,
        .backlight_gpio     = config->backlight_gpio,
        .lane_bit_rate_mbps = config->pixel_format == BSP_PIXEL_FORMAT_RGB888 ? 870 : 800,
        .dpi_clock_freq_mhz = 75,
        .video_timing = {
            .hsync_pulse_width = 40,
            .hsync_back_porch  = 140,
            .hsync_front_porch = 40,
            .vsync_pulse_width = 4,
            .vsync_back_porch  = 20,
            .vsync_front_porch = 20,
        },
        .panel_init = send_init_sequence,
    };
    return mipi_dsi_lcd_create(&mipi_config, out);
}
