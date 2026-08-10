/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Generic MIPI-DSI LCD provider. This driver owns the PHY LDO, DSI bus, DBI
 * command IO, DPI panel, host framebuffers, and optional LEDC backlight. Panel
 * drivers supply transport/timing parameters and an initialization callback;
 * the callback remains responsible for the complete panel command sequence.
 */

#pragma once
#include "bsp_display.h"
#include "driver/gpio.h"
#include "esp_lcd_mipi_dsi.h"

#ifdef __cplusplus
extern "C" {
#endif

#define MIPI_DSI_MAX_FRAME_BUFFERS BSP_DISPLAY_MAX_FRAME_BUFFERS

typedef struct {
    int          cmd;
    const void  *data;
    size_t       data_bytes;
    uint32_t     delay_ms;
} mipi_dsi_init_cmd_t;

typedef esp_err_t (*mipi_dsi_panel_init_fn_t)(esp_lcd_panel_io_handle_t io,
                                               bsp_pixel_format_t pixel_format,
                                               void *user_data);

typedef struct {
    bsp_size_t         size;
    bsp_pixel_format_t pixel_format; /*!< RGB565 or RGB888 */
    uint8_t            fb_num;       /*!< 0 selects one framebuffer; maximum is 3 */
    gpio_num_t         backlight_gpio; /*!< NC disables backlight control */

    float                  lane_bit_rate_mbps;
    float                  dpi_clock_freq_mhz;
    esp_lcd_video_timing_t video_timing; /*!< h_size/v_size are replaced by size */

    mipi_dsi_panel_init_fn_t panel_init;
    void                    *user_data;
} mipi_dsi_config_t;

/* Sends `count` commands in order and applies each command's post-delay. */
BSP_NONNULL(1) esp_err_t mipi_dsi_send_init_cmds(esp_lcd_panel_io_handle_t io,
                                                  const mipi_dsi_init_cmd_t *cmds,
                                                  size_t count);

BSP_NONNULL(1, 2) esp_err_t mipi_dsi_lcd_create(const mipi_dsi_config_t *config,
                                                 bsp_display_t **out);

#ifdef __cplusplus
}
#endif
