/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "rgb_lcd.h"
#include "bsp_display.h"
#include "driver/ledc.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_panel_rgb.h"
#include "esp_log.h"
#include <stdlib.h>

static const char *TAG = "rgb_lcd";

typedef struct {
    bsp_display_t base;                 /* must be first — struct-inheritance vtable */
    bool                       has_backlight;
    ledc_channel_config_t      ledc_channel;
    esp_lcd_panel_handle_t     panel;
    uint8_t fb_num;
    void   *frame_buffers[3];
} rgb_lcd_t;

static esp_err_t draw_bitmap(bsp_display_t *self, bsp_rect_t rect, const void *data,
                             bsp_rotation_t rotation) {
    rgb_lcd_t *d = (rgb_lcd_t *)self;
    /* Fast path is the framebuffer flush; draw_bitmap is the partial fallback. */
    if (rotation != BSP_ROTATION_0) return ESP_ERR_NOT_SUPPORTED;
    return esp_lcd_panel_draw_bitmap(d->panel,
        bsp_rect_min_x(rect), bsp_rect_min_y(rect),
        bsp_rect_max_x(rect), bsp_rect_max_y(rect), data);
}

static esp_err_t flush(bsp_display_t *self, int fb_index) {
    rgb_lcd_t *d = (rgb_lcd_t *)self;
    if (fb_index < 0 || fb_index >= d->fb_num) return ESP_ERR_INVALID_ARG;
    if (d->frame_buffers[fb_index] == NULL) return ESP_ERR_INVALID_STATE;
    return esp_lcd_panel_draw_bitmap(d->panel, 0, 0,
        self->size.width, self->size.height, d->frame_buffers[fb_index]);
}

static void **get_framebuffers(bsp_display_t *self) {
    return ((rgb_lcd_t *)self)->frame_buffers;
}

static esp_err_t set_brightness(bsp_display_t *self, int brightness) {
    rgb_lcd_t *d = (rgb_lcd_t *)self;
    if (!d->has_backlight) return ESP_OK;
    if (brightness < 0)   brightness = 0;
    if (brightness > 100) brightness = 100;
    uint32_t duty = (uint32_t)(((float)brightness / 100.0f) * ((1 << 12) - 1));
    esp_err_t ret = ledc_set_duty(d->ledc_channel.speed_mode, d->ledc_channel.channel, duty);
    if (ret != ESP_OK) return ret;
    return ledc_update_duty(d->ledc_channel.speed_mode, d->ledc_channel.channel);
}

static esp_err_t deinit(bsp_display_t *self) {
    rgb_lcd_t *d = (rgb_lcd_t *)self;
    if (d->panel) esp_lcd_panel_del(d->panel);
    free(d);
    return ESP_OK;
}

esp_err_t rgb_lcd_create(const rgb_lcd_config_t *config, bsp_display_t **out) {
    esp_err_t ret;

    if (config->pixel_format != BSP_PIXEL_FORMAT_RGB565) {
        ESP_LOGE(TAG, "only RGB565 supported");
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (config->data_width == 0 || config->data_width > RGB_LCD_MAX_DATA_LINES) {
        return ESP_ERR_INVALID_ARG;
    }

    rgb_lcd_t *state = calloc(1, sizeof(rgb_lcd_t));
    if (!state) return ESP_ERR_NO_MEM;

    state->base = (bsp_display_t){
        .type            = BSP_DISPLAY_TYPE_RGB,
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

    esp_lcd_rgb_panel_config_t panel_config = {
        .clk_src        = LCD_CLK_SRC_DEFAULT,
        .data_width     = config->data_width,
        .in_color_format = LCD_COLOR_FMT_RGB565,
        .num_fbs        = state->fb_num,
        .dma_burst_size = config->dma_burst_size ? config->dma_burst_size : 64,
        .hsync_gpio_num = config->hsync_gpio,
        .vsync_gpio_num = config->vsync_gpio,
        .de_gpio_num    = config->de_gpio,
        .pclk_gpio_num  = config->pclk_gpio,
        .disp_gpio_num  = config->disp_gpio,
        .timings = {
            .pclk_hz           = config->pclk_hz,
            .h_res             = config->size.width,
            .v_res             = config->size.height,
            .hsync_pulse_width = config->timing.hsync_pulse_width,
            .hsync_back_porch  = config->timing.hsync_back_porch,
            .hsync_front_porch = config->timing.hsync_front_porch,
            .vsync_pulse_width = config->timing.vsync_pulse_width,
            .vsync_back_porch  = config->timing.vsync_back_porch,
            .vsync_front_porch = config->timing.vsync_front_porch,
            .flags = {
                .pclk_active_neg = config->timing.pclk_active_neg,
            },
        },
        .flags = {
            .fb_in_psram = config->fb_in_psram,
        },
    };
    for (size_t i = 0; i < ESP_LCD_RGB_BUS_WIDTH_MAX; i++) {
        panel_config.data_gpio_nums[i] =
            (i < RGB_LCD_MAX_DATA_LINES) ? config->data_gpios[i] : GPIO_NUM_NC;
    }

    ret = esp_lcd_new_rgb_panel(&panel_config, &state->panel);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "esp_lcd_new_rgb_panel: %s", esp_err_to_name(ret));
        goto err_free;
    }
    ret = esp_lcd_panel_reset(state->panel);
    if (ret != ESP_OK) goto err_panel;
    ret = esp_lcd_panel_init(state->panel);
    if (ret != ESP_OK) goto err_panel;

    void *fb0 = NULL, *fb1 = NULL, *fb2 = NULL;
    esp_lcd_rgb_panel_get_frame_buffer(state->panel, state->fb_num, &fb0, &fb1, &fb2);
    state->frame_buffers[0] = fb0;
    state->frame_buffers[1] = fb1;
    state->frame_buffers[2] = fb2;

    *out = &state->base;
    return ESP_OK;

err_panel:
    esp_lcd_panel_del(state->panel);
err_free:
    free(state);
    return ret;
}
