/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Low-level direct-drive EPD waveform driver for ESP32-S3.
 */

#include "epd_ll.h"
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_io_i80.h"
#include "driver/gpio.h"
#include "esp_rom_sys.h"
#include "esp_attr.h"
#include "esp_check.h"

static const char *TAG = "epd_ll";

static esp_lcd_i80_bus_handle_t   s_bus;
static esp_lcd_panel_io_handle_t  s_io;
static epd_ll_config_t            s_cfg;
static volatile bool              s_busy;   /* a scanline DMA is in flight */

static IRAM_ATTR bool on_trans_done(esp_lcd_panel_io_handle_t io,
                                    esp_lcd_panel_io_event_data_t *edata,
                                    void *user_ctx) {
    gpio_set_level(s_cfg.ckv_pin, 0);
    gpio_set_level(s_cfg.le_pin, 1);
    s_busy = false;
    return false;
}

esp_err_t epd_ll_init(const epd_ll_config_t *cfg) {
    s_cfg = *cfg;
    s_busy = false;

    /* Bit-banged control lines CKV/SPV (gate) + LE (latch), all idle low. */
    gpio_config_t gc = {
        .pin_bit_mask = (1ULL << cfg->ckv_pin) | (1ULL << cfg->spv_pin) | (1ULL << cfg->le_pin),
        .mode         = GPIO_MODE_OUTPUT,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    ESP_RETURN_ON_ERROR(gpio_config(&gc), TAG, "ctrl gpio");
    gpio_set_level(cfg->ckv_pin, 0);
    gpio_set_level(cfg->spv_pin, 0);
    gpio_set_level(cfg->le_pin, 0);

    esp_lcd_i80_bus_config_t bus_cfg = {
        .dc_gpio_num        = cfg->dc_dummy_pin,
        .wr_gpio_num        = cfg->cl_pin,
        .clk_src            = LCD_CLK_SRC_PLL160M,
        .bus_width          = 8,
        .max_transfer_bytes = cfg->line_bytes + cfg->line_padding,
        .dma_burst_size     = 64,
    };
    for (int i = 0; i < 8; i++) {
        bus_cfg.data_gpio_nums[i] = cfg->data_pins[i];
        gpio_set_direction(cfg->data_pins[i], GPIO_MODE_OUTPUT);
    }
    gpio_set_direction(cfg->cl_pin, GPIO_MODE_OUTPUT);
    gpio_set_direction(cfg->sph_pin, GPIO_MODE_OUTPUT);
    ESP_RETURN_ON_ERROR(esp_lcd_new_i80_bus(&bus_cfg, &s_bus), TAG, "i80 bus");

    /* Reclaim the dummy D/C pin as a plain GPIO output (caller sets its level).
     * gpio_set_direction alone re-routes the pad; gpio_reset_pin would enable the
     * pull-up and briefly drive it -- a stray glitch on the PWR pin. */
    gpio_set_direction(cfg->dc_dummy_pin, GPIO_MODE_OUTPUT);

    esp_lcd_panel_io_i80_config_t io_cfg = {
        .cs_gpio_num         = cfg->sph_pin,
        .pclk_hz             = cfg->pclk_hz,
        .trans_queue_depth   = 4,
        .on_color_trans_done = on_trans_done,
        .lcd_cmd_bits        = 0,
        .lcd_param_bits      = 0,
        .dc_levels = { .dc_idle_level = 0, .dc_cmd_level = 0, .dc_dummy_level = 0, .dc_data_level = 1 },
        .flags     = { .pclk_idle_low = 1 },
    };
    ESP_RETURN_ON_ERROR(esp_lcd_new_panel_io_i80(s_bus, &io_cfg, &s_io), TAG, "i80 io");

    return ESP_OK;
}

size_t epd_ll_line_buf_size(void) {
    return s_cfg.line_bytes + s_cfg.line_padding;
}

void epd_ll_frame_begin(void) {
    const int ckv = s_cfg.ckv_pin, spv = s_cfg.spv_pin;
    while (s_busy) taskYIELD();
    gpio_set_level(spv, 0); esp_rom_delay_us(1);
    gpio_set_level(ckv, 0); esp_rom_delay_us(3);
    gpio_set_level(ckv, 1); esp_rom_delay_us(1);
    gpio_set_level(spv, 1);
    for (int i = 0; i < 3; i++) {
        esp_rom_delay_us(3);
        gpio_set_level(ckv, 0);
        esp_rom_delay_us(3);
        gpio_set_level(ckv, 1);
    }
}

void epd_ll_write_line(const uint8_t *data) {
    while (s_busy) taskYIELD();           /* wait previous scanline DMA + its latch */
    s_busy = true;
    gpio_set_level(s_cfg.le_pin, 0);      /* close the latch before shifting */
    gpio_set_level(s_cfg.ckv_pin, 1);     /* CKV high for this line */
    esp_lcd_panel_io_tx_color(s_io, -1, data, s_cfg.line_bytes + s_cfg.line_padding);
}

void epd_ll_frame_end(void) {
    while (s_busy) taskYIELD();           /* wait last line (the ISR already latched it) */
    gpio_set_level(s_cfg.le_pin, 0);
    gpio_set_level(s_cfg.ckv_pin, 1);
}
