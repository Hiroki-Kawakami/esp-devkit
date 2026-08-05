/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "mipi_dsi.h"
#include "driver/ledc.h"
#include "esp_ldo_regulator.h"
#include "esp_lcd_panel_commands.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include <stdlib.h>

static const char *TAG = "mipi_dsi";

typedef struct {
    bsp_display_t base; /* must be first -- struct-inheritance vtable */
    esp_ldo_channel_handle_t   phy_power_channel;
    esp_lcd_dsi_bus_handle_t   dsi_bus;
    esp_lcd_panel_io_handle_t  io;
    esp_lcd_panel_handle_t     panel;
    uint8_t                    fb_num;
    void                      *frame_buffers[MIPI_DSI_MAX_FRAME_BUFFERS];
    bool                       asleep;
} mipi_dsi_lcd_t;

esp_err_t mipi_dsi_send_init_cmds(esp_lcd_panel_io_handle_t io,
                                  const mipi_dsi_init_cmd_t *cmds,
                                  size_t count) {
    if (!io || (count > 0 && !cmds)) return ESP_ERR_INVALID_ARG;

    for (size_t i = 0; i < count; i++) {
        if (cmds[i].data_bytes > 0 && !cmds[i].data) return ESP_ERR_INVALID_ARG;

        esp_err_t err = esp_lcd_panel_io_tx_param(io, cmds[i].cmd,
                                                   cmds[i].data, cmds[i].data_bytes);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "init cmd 0x%02x failed: %s", cmds[i].cmd,
                     esp_err_to_name(err));
            return err;
        }
        if (cmds[i].delay_ms > 0) {
            vTaskDelay(pdMS_TO_TICKS(cmds[i].delay_ms));
        }
    }
    return ESP_OK;
}

static esp_err_t draw_bitmap(bsp_display_t *self, bsp_rect_t rect, const void *data,
                             bsp_rotation_t rotation) {
    mipi_dsi_lcd_t *lcd = (mipi_dsi_lcd_t *)self;
    if (rotation != BSP_ROTATION_0) return ESP_ERR_NOT_SUPPORTED;
    return esp_lcd_panel_draw_bitmap(lcd->panel,
        bsp_rect_min_x(rect), bsp_rect_min_y(rect),
        bsp_rect_max_x(rect), bsp_rect_max_y(rect), data);
}

static esp_err_t flush(bsp_display_t *self, int fb_index) {
    mipi_dsi_lcd_t *lcd = (mipi_dsi_lcd_t *)self;
    if (fb_index < 0 || fb_index >= lcd->fb_num) return ESP_ERR_INVALID_ARG;
    if (!lcd->frame_buffers[fb_index]) return ESP_ERR_INVALID_STATE;
    return esp_lcd_panel_draw_bitmap(lcd->panel, 0, 0,
        self->size.width, self->size.height, lcd->frame_buffers[fb_index]);
}

static void **get_framebuffers(bsp_display_t *self) {
    return ((mipi_dsi_lcd_t *)self)->frame_buffers;
}

static esp_err_t set_brightness(bsp_display_t *self, int brightness) {
    (void)self;
    if (brightness < 0) brightness = 0;
    if (brightness > 100) brightness = 100;
    uint32_t duty = (uint32_t)(((float)brightness / 100.0f) * ((1 << 12) - 1));

    esp_err_t err = ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0, duty);
    if (err != ESP_OK) return err;
    return ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0);
}

static esp_err_t set_power(bsp_display_t *self, bsp_display_power_t state) {
    mipi_dsi_lcd_t *lcd = (mipi_dsi_lcd_t *)self;
    bool sleep = state != BSP_DISPLAY_POWER_ON;
    if (sleep == lcd->asleep) return ESP_OK;

    esp_err_t err;
    if (sleep) {
        if ((err = esp_lcd_panel_io_tx_param(lcd->io, LCD_CMD_DISPOFF, NULL, 0)) != ESP_OK) {
            return err;
        }
        if ((err = esp_lcd_panel_io_tx_param(lcd->io, LCD_CMD_SLPIN, NULL, 0)) != ESP_OK) {
            return err;
        }
        vTaskDelay(pdMS_TO_TICKS(5));
    } else {
        if ((err = esp_lcd_panel_io_tx_param(lcd->io, LCD_CMD_SLPOUT, NULL, 0)) != ESP_OK) {
            return err;
        }
        vTaskDelay(pdMS_TO_TICKS(120));
        if ((err = esp_lcd_panel_io_tx_param(lcd->io, LCD_CMD_DISPON, NULL, 0)) != ESP_OK) {
            return err;
        }
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    lcd->asleep = sleep;
    return ESP_OK;
}

static esp_err_t deinit(bsp_display_t *self) {
    mipi_dsi_lcd_t *lcd = (mipi_dsi_lcd_t *)self;

    if (self->set_brightness) {
        self->set_brightness(self, 0);
        ledc_stop(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0, 0);
    }
    if (lcd->panel) esp_lcd_panel_del(lcd->panel);
    if (lcd->io) esp_lcd_panel_io_del(lcd->io);
    if (lcd->dsi_bus) esp_lcd_del_dsi_bus(lcd->dsi_bus);
    if (lcd->phy_power_channel) esp_ldo_release_channel(lcd->phy_power_channel);
    free(lcd);
    return ESP_OK;
}

static esp_err_t validate_config(const mipi_dsi_config_t *config) {
    if (config->size.width <= 0 || config->size.height <= 0) return ESP_ERR_INVALID_ARG;
    if (config->pixel_format != BSP_PIXEL_FORMAT_RGB565 &&
        config->pixel_format != BSP_PIXEL_FORMAT_RGB888) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (config->fb_num > MIPI_DSI_MAX_FRAME_BUFFERS) return ESP_ERR_INVALID_ARG;
    if (config->lane_bit_rate_mbps <= 0 || config->dpi_clock_freq_mhz <= 0) {
        return ESP_ERR_INVALID_ARG;
    }
    if (!config->panel_init) return ESP_ERR_INVALID_ARG;
    return ESP_OK;
}

esp_err_t mipi_dsi_lcd_create(const mipi_dsi_config_t *config, bsp_display_t **out) {
    if (!config || !out) return ESP_ERR_INVALID_ARG;
    *out = NULL;

    esp_err_t ret = validate_config(config);
    if (ret != ESP_OK) return ret;

    mipi_dsi_lcd_t *lcd = calloc(1, sizeof(*lcd));
    if (!lcd) return ESP_ERR_NO_MEM;

    lcd->base = (bsp_display_t){
        .type             = BSP_DISPLAY_TYPE_MIPI_DSI,
        .size             = config->size,
        .format           = config->pixel_format,
        .draw_bitmap      = draw_bitmap,
        .deinit           = deinit,
        .set_brightness   = NULL,
        .set_power        = set_power,
        .get_framebuffers = get_framebuffers,
        .flush            = flush,
    };
    lcd->fb_num = config->fb_num ? config->fb_num : 1;

    if (config->backlight_gpio != GPIO_NUM_NC) {
        const ledc_timer_config_t timer_config = {
            .speed_mode      = LEDC_LOW_SPEED_MODE,
            .duty_resolution = LEDC_TIMER_12_BIT,
            .timer_num       = LEDC_TIMER_0,
            .freq_hz         = 5000,
            .clk_cfg         = LEDC_AUTO_CLK,
        };
        ret = ledc_timer_config(&timer_config);
        if (ret != ESP_OK) goto err_free;

        const ledc_channel_config_t channel_config = {
            .gpio_num   = config->backlight_gpio,
            .speed_mode = LEDC_LOW_SPEED_MODE,
            .channel    = LEDC_CHANNEL_0,
            .timer_sel  = LEDC_TIMER_0,
            .duty       = 0,
            .hpoint     = 0,
        };
        ret = ledc_channel_config(&channel_config);
        if (ret != ESP_OK) goto err_free;
        lcd->base.set_brightness = set_brightness;
    }

    const esp_ldo_channel_config_t ldo_config = {
        .chan_id    = 3,
        .voltage_mv = 2500,
    };
    ret = esp_ldo_acquire_channel(&ldo_config, &lcd->phy_power_channel);
    if (ret != ESP_OK) goto err_backlight;

    const esp_lcd_dsi_bus_config_t bus_config = {
        .bus_id             = 0,
        .num_data_lanes     = 2,
        .phy_clk_src        = MIPI_DSI_PHY_CLK_SRC_DEFAULT,
        .lane_bit_rate_mbps = config->lane_bit_rate_mbps,
    };
    ret = esp_lcd_new_dsi_bus(&bus_config, &lcd->dsi_bus);
    if (ret != ESP_OK) goto err_ldo;

    const esp_lcd_dbi_io_config_t dbi_config = {
        .virtual_channel = 0,
        .lcd_cmd_bits    = 8,
        .lcd_param_bits  = 8,
    };
    ret = esp_lcd_new_panel_io_dbi(lcd->dsi_bus, &dbi_config, &lcd->io);
    if (ret != ESP_OK) goto err_dsi_bus;

    const lcd_color_format_t color_format =
        config->pixel_format == BSP_PIXEL_FORMAT_RGB888
        ? LCD_COLOR_FMT_RGB888 : LCD_COLOR_FMT_RGB565;
    esp_lcd_video_timing_t video_timing = config->video_timing;
    video_timing.h_size = config->size.width;
    video_timing.v_size = config->size.height;
    const esp_lcd_dpi_panel_config_t dpi_config = {
        .virtual_channel    = 0,
        .dpi_clk_src        = MIPI_DSI_DPI_CLK_SRC_DEFAULT,
        .dpi_clock_freq_mhz = config->dpi_clock_freq_mhz,
        .in_color_format    = color_format,
        .out_color_format   = color_format,
        .num_fbs            = lcd->fb_num,
        .video_timing       = video_timing,
    };
    ret = esp_lcd_new_panel_dpi(lcd->dsi_bus, &dpi_config, &lcd->panel);
    if (ret != ESP_OK) goto err_io;

    ret = esp_lcd_dpi_panel_enable_dma2d(lcd->panel);
    if (ret != ESP_OK) goto err_panel;

    ret = config->panel_init(lcd->io, config->pixel_format, config->user_data);
    if (ret != ESP_OK) goto err_panel;

    ret = esp_lcd_panel_init(lcd->panel);
    if (ret != ESP_OK) goto err_panel;

    void *fb0 = NULL, *fb1 = NULL, *fb2 = NULL;
    esp_lcd_dpi_panel_get_frame_buffer(lcd->panel, lcd->fb_num, &fb0, &fb1, &fb2);
    lcd->frame_buffers[0] = fb0;
    lcd->frame_buffers[1] = fb1;
    lcd->frame_buffers[2] = fb2;

    *out = &lcd->base;
    return ESP_OK;

err_panel:
    esp_lcd_panel_del(lcd->panel);
err_io:
    esp_lcd_panel_io_del(lcd->io);
err_dsi_bus:
    esp_lcd_del_dsi_bus(lcd->dsi_bus);
err_ldo:
    esp_ldo_release_channel(lcd->phy_power_channel);
err_backlight:
    if (lcd->base.set_brightness) {
        ledc_stop(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0, 0);
    }
err_free:
    free(lcd);
    return ret;
}
