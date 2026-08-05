/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "st7123_lcd.h"
#include "st7123_init_data.h"
#include "mipi_dsi.h"
#include "esp_lcd_panel_commands.h"

static esp_err_t send_init_sequence(esp_lcd_panel_io_handle_t io,
                                    bsp_pixel_format_t pixel_format,
                                    void *user_data) {
    (void)pixel_format;
    (void)user_data;

    const mipi_dsi_init_cmd_t prefix[] = {
        { LCD_CMD_SWRESET, NULL, 0, 120 },
    };
    esp_err_t err = mipi_dsi_send_init_cmds(io, prefix, sizeof(prefix) / sizeof(prefix[0]));
    if (err != ESP_OK) return err;
    return mipi_dsi_send_init_cmds(io, st7123_init_cmds, st7123_init_cmds_count);
}

esp_err_t st7123_lcd_create(const st7123_config_t *config, bsp_display_t **out) {
    const mipi_dsi_config_t mipi_config = {
        .size               = config->size,
        .pixel_format       = config->pixel_format,
        .fb_num             = config->fb_num,
        .backlight_gpio     = config->backlight_gpio,
        .lane_bit_rate_mbps = 965,
        .dpi_clock_freq_mhz = 75,
        .video_timing = {
            .hsync_pulse_width = 2,
            .hsync_back_porch  = 80,
            .hsync_front_porch = 40,
            .vsync_pulse_width = 2,
            .vsync_back_porch  = 8,
            .vsync_front_porch = 220,
        },
        .panel_init = send_init_sequence,
    };
    return mipi_dsi_lcd_create(&mipi_config, out);
}
