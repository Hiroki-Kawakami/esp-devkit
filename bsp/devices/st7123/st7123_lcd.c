/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "st7123_lcd.h"
#include "st7123_init_data.h"
#include "bsp_display.h"
#include "driver/ledc.h"
#include "esp_ldo_regulator.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_mipi_dsi.h"
#include "esp_lcd_panel_commands.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include <stdlib.h>

static const char *TAG = "st7123";

typedef struct {
    bsp_display_t base;                 /* must be first — struct-inheritance vtable */
    bool                       has_backlight;
    ledc_channel_config_t      ledc_channel;
    esp_ldo_channel_handle_t   phy_power_channel;
    esp_lcd_dsi_bus_handle_t   mipi_dsi_bus;
    esp_lcd_panel_io_handle_t  io;
    esp_lcd_panel_handle_t     panel;
    uint8_t fb_num;
    void   *frame_buffers[3];
} st7123_lcd_t;

static esp_err_t draw_bitmap(bsp_display_t *self, bsp_rect_t rect, const void *data,
                             bsp_rotation_t rotation) {
    st7123_lcd_t *d = (st7123_lcd_t *)self;
    if (rotation != BSP_ROTATION_0) return ESP_ERR_NOT_SUPPORTED;
    return esp_lcd_panel_draw_bitmap(d->panel,
        bsp_rect_min_x(rect), bsp_rect_min_y(rect),
        bsp_rect_max_x(rect), bsp_rect_max_y(rect), data);
}

static esp_err_t flush(bsp_display_t *self, int fb_index) {
    st7123_lcd_t *d = (st7123_lcd_t *)self;
    if (fb_index < 0 || fb_index >= d->fb_num) return ESP_ERR_INVALID_ARG;
    if (d->frame_buffers[fb_index] == NULL) return ESP_ERR_INVALID_STATE;
    return esp_lcd_panel_draw_bitmap(d->panel, 0, 0,
        self->size.width, self->size.height, d->frame_buffers[fb_index]);
}

static void **get_framebuffers(bsp_display_t *self) {
    return ((st7123_lcd_t *)self)->frame_buffers;
}

static esp_err_t set_brightness(bsp_display_t *self, int brightness) {
    st7123_lcd_t *d = (st7123_lcd_t *)self;
    if (!d->has_backlight) return ESP_OK;
    if (brightness < 0)   brightness = 0;
    if (brightness > 100) brightness = 100;
    uint32_t duty = (uint32_t)(((float)brightness / 100.0f) * ((1 << 12) - 1));
    esp_err_t ret = ledc_set_duty(d->ledc_channel.speed_mode, d->ledc_channel.channel, duty);
    if (ret != ESP_OK) return ret;
    return ledc_update_duty(d->ledc_channel.speed_mode, d->ledc_channel.channel);
}

static esp_err_t deinit(bsp_display_t *self) {
    st7123_lcd_t *d = (st7123_lcd_t *)self;
    if (d->panel)             esp_lcd_panel_del(d->panel);
    if (d->io)                esp_lcd_panel_io_del(d->io);
    if (d->mipi_dsi_bus)      esp_lcd_del_dsi_bus(d->mipi_dsi_bus);
    if (d->phy_power_channel) esp_ldo_release_channel(d->phy_power_channel);
    free(d);
    return ESP_OK;
}

/* Send the chip's init sequence over DBI; must run before esp_lcd_panel_init()
 * starts the DPI video stream. The vendor sequence ends with MADCTL / SLPOUT /
 * DISPON, so we only need a software reset up front. */
static esp_err_t send_init_sequence(esp_lcd_panel_io_handle_t io) {
    esp_err_t err;

    if ((err = esp_lcd_panel_io_tx_param(io, LCD_CMD_SWRESET, NULL, 0)) != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(120));

    for (size_t i = 0; i < st7123_init_cmds_count; i++) {
        err = esp_lcd_panel_io_tx_param(io, st7123_init_cmds[i].cmd,
                                        st7123_init_cmds[i].data,
                                        st7123_init_cmds[i].data_bytes);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "init cmd 0x%02x failed: %s", st7123_init_cmds[i].cmd,
                     esp_err_to_name(err));
            return err;
        }
        if (st7123_init_cmds[i].delay_ms) {
            vTaskDelay(pdMS_TO_TICKS(st7123_init_cmds[i].delay_ms));
        }
    }
    return ESP_OK;
}

esp_err_t st7123_lcd_create(const st7123_config_t *config, bsp_display_t **out) {
    esp_err_t ret;

    st7123_lcd_t *state = calloc(1, sizeof(st7123_lcd_t));
    if (!state) return ESP_ERR_NO_MEM;

    state->base = (bsp_display_t){
        .type            = BSP_DISPLAY_TYPE_MIPI_DSI,
        .size            = config->size,
        .format          = config->pixel_format,
        .draw_bitmap     = draw_bitmap,
        .deinit          = deinit,
        .set_brightness  = set_brightness,
        .get_framebuffers = get_framebuffers,
        .flush           = flush,
    };
    state->fb_num = config->fb_num > 0 ? config->fb_num : 1;
    if (state->fb_num > 3) state->fb_num = 3;

    const bool rgb888 = (config->pixel_format == BSP_PIXEL_FORMAT_RGB888);

    if (config->backlight_gpio != GPIO_NUM_NC) {
        const ledc_timer_config_t timer_cfg = {
            .speed_mode      = LEDC_LOW_SPEED_MODE,
            .duty_resolution = LEDC_TIMER_12_BIT,
            .timer_num       = LEDC_TIMER_0,
            .freq_hz         = 5000,
            .clk_cfg         = LEDC_AUTO_CLK,
        };
        ret = ledc_timer_config(&timer_cfg);
        if (ret != ESP_OK) goto err_free;
        state->ledc_channel = (ledc_channel_config_t){
            .gpio_num   = config->backlight_gpio,
            .speed_mode = LEDC_LOW_SPEED_MODE,
            .channel    = LEDC_CHANNEL_0,
            .timer_sel  = LEDC_TIMER_0,
            .duty       = 0,
            .hpoint     = 0,
        };
        ret = ledc_channel_config(&state->ledc_channel);
        if (ret != ESP_OK) goto err_free;
        state->has_backlight = true;
    }

    esp_ldo_channel_config_t ldo_config = { .chan_id = 3, .voltage_mv = 2500 };
    ret = esp_ldo_acquire_channel(&ldo_config, &state->phy_power_channel);
    if (ret != ESP_OK) goto err_free;

    esp_lcd_dsi_bus_config_t bus_config = {
        .bus_id             = 0,
        .num_data_lanes     = 2,
        .phy_clk_src        = MIPI_DSI_PHY_CLK_SRC_DEFAULT,
        .lane_bit_rate_mbps = 965,
    };
    ret = esp_lcd_new_dsi_bus(&bus_config, &state->mipi_dsi_bus);
    if (ret != ESP_OK) goto err_ldo;

    esp_lcd_dbi_io_config_t dbi_config = {
        .virtual_channel = 0,
        .lcd_cmd_bits    = 8,
        .lcd_param_bits  = 8,
    };
    ret = esp_lcd_new_panel_io_dbi(state->mipi_dsi_bus, &dbi_config, &state->io);
    if (ret != ESP_OK) goto err_dsi_bus;

    esp_lcd_dpi_panel_config_t dpi_config = {
        .virtual_channel    = 0,
        .dpi_clk_src        = MIPI_DSI_DPI_CLK_SRC_DEFAULT,
        .dpi_clock_freq_mhz = 75,
        .pixel_format       = rgb888 ? LCD_COLOR_PIXEL_FORMAT_RGB888 : LCD_COLOR_PIXEL_FORMAT_RGB565,
        .num_fbs            = state->fb_num,
        .video_timing = {
            .h_size            = config->size.width,
            .v_size            = config->size.height,
            .hsync_pulse_width = 2,
            .hsync_back_porch  = 140,
            .hsync_front_porch = 40,
            .vsync_pulse_width = 2,
            .vsync_back_porch  = 8,
            .vsync_front_porch = 220,
        },
        .flags = { .use_dma2d = 1 },
    };
    ret = esp_lcd_new_panel_dpi(state->mipi_dsi_bus, &dpi_config, &state->panel);
    if (ret != ESP_OK) goto err_io;

    ret = send_init_sequence(state->io);
    if (ret != ESP_OK) goto err_panel;

    ret = esp_lcd_panel_init(state->panel);
    if (ret != ESP_OK) goto err_panel;

    void *fb0 = NULL, *fb1 = NULL, *fb2 = NULL;
    esp_lcd_dpi_panel_get_frame_buffer(state->panel, state->fb_num, &fb0, &fb1, &fb2);
    state->frame_buffers[0] = fb0;
    state->frame_buffers[1] = fb1;
    state->frame_buffers[2] = fb2;

    *out = &state->base;
    return ESP_OK;

err_panel:
    esp_lcd_panel_del(state->panel);
err_io:
    esp_lcd_panel_io_del(state->io);
err_dsi_bus:
    esp_lcd_del_dsi_bus(state->mipi_dsi_bus);
err_ldo:
    esp_ldo_release_channel(state->phy_power_channel);
err_free:
    free(state);
    return ret;
}
