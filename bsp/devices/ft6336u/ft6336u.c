/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ft6336u.h"
#include <stdlib.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "ft6336u";

#define I2C_TIMEOUT_MS  100

/* Registers. */
#define REG_TD_STATUS   0x02   /* [3:0] = number of touch points */
#define REG_P1_XH       0x03   /* first point; 6 bytes/point, XH XL YH YL W MISC */
#define REG_G_MODE      0xA4   /* INT mode: 0 = polling, 1 = trigger */
#define REG_CHIP_ID     0xA3
#define POINT_STRIDE    6

#define G_MODE_TRIGGER  0x01

struct ft6336u_dev {
    bsp_touch_t base;
    i2c_master_dev_handle_t i2c;
};

static esp_err_t read_regs(struct ft6336u_dev *dev, uint8_t reg, uint8_t *buf, size_t len) {
    return i2c_master_transmit_receive(dev->i2c, &reg, 1, buf, len, I2C_TIMEOUT_MS);
}

static esp_err_t write_reg(struct ft6336u_dev *dev, uint8_t reg, uint8_t val) {
    uint8_t buf[2] = { reg, val };
    return i2c_master_transmit(dev->i2c, buf, sizeof(buf), I2C_TIMEOUT_MS);
}

static esp_err_t ft6336u_poll(bsp_touch_t *self, bsp_touch_raw_point_t *out, uint8_t max,
                              uint8_t *count, bool *keep_polling) {
    struct ft6336u_dev *dev = (struct ft6336u_dev *)self;
    (void)keep_polling;

    uint8_t buf[1 + FT6336U_MAX_TOUCH_POINTS * POINT_STRIDE];
    esp_err_t err = read_regs(dev, REG_TD_STATUS, buf, sizeof(buf));
    if (err != ESP_OK) { *count = 0; return err; }

    uint8_t n = buf[0] & 0x0F;
    if (n > FT6336U_MAX_TOUCH_POINTS) n = FT6336U_MAX_TOUCH_POINTS;
    if (n > max) n = max;
    for (uint8_t i = 0; i < n; i++) {
        const uint8_t *p = &buf[1 + i * POINT_STRIDE];   /* XH XL YH YL */
        out[i].x  = (uint16_t)((p[0] & 0x0F) << 8) | p[1];
        out[i].y  = (uint16_t)((p[2] & 0x0F) << 8) | p[3];
        out[i].id = p[2] >> 4;   /* YH[7:4] = touch id */
    }
    *count = n;
    return ESP_OK;
}

static esp_err_t ft6336u_deinit(bsp_touch_t *self) {
    struct ft6336u_dev *dev = (struct ft6336u_dev *)self;
    if (!dev) return ESP_OK;
    if (dev->i2c) i2c_master_bus_rm_device(dev->i2c);
    free(dev);
    return ESP_OK;
}

static void hw_reset(gpio_num_t reset_io) {
    if (reset_io == GPIO_NUM_NC) return;
    const gpio_config_t out = { .pin_bit_mask = 1ULL << reset_io, .mode = GPIO_MODE_OUTPUT };
    gpio_config(&out);
    gpio_set_level(reset_io, 0);   vTaskDelay(pdMS_TO_TICKS(5));
    gpio_set_level(reset_io, 1);   vTaskDelay(pdMS_TO_TICKS(200));   /* boot time */
}

esp_err_t ft6336u_touch_create(const ft6336u_config_t *config, bsp_touch_t **out_touch) {
    if (!config || !config->i2c_bus || !out_touch) return ESP_ERR_INVALID_ARG;

    struct ft6336u_dev *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;

    hw_reset(config->reset_io);

    const i2c_device_config_t cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = config->i2c_address ? config->i2c_address : FT6336U_I2C_ADDR,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : FT6336U_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(config->i2c_bus, &cfg, &dev->i2c);
    if (err != ESP_OK) { free(dev); return err; }

    uint8_t id;
    if (read_regs(dev, REG_CHIP_ID, &id, 1) != ESP_OK) {
        ESP_LOGE(TAG, "not responding on 0x%02x", cfg.device_address);
        i2c_master_bus_rm_device(dev->i2c);
        free(dev);
        return ESP_ERR_NOT_FOUND;
    }

    /* Interrupt trigger mode: INT idles high and pulses on a touch change (the
     * default polling mode holds INT low, which stalls an edge-driven host). */
    write_reg(dev, REG_G_MODE, G_MODE_TRIGGER);

    dev->base.poll       = ft6336u_poll;
    dev->base.deinit     = ft6336u_deinit;
    dev->base.width      = config->width;
    dev->base.height     = config->height;
    dev->base.max_points = FT6336U_MAX_TOUCH_POINTS;
    dev->base.swap_xy    = config->swap_xy;
    dev->base.mirror_x   = config->mirror_x;
    dev->base.mirror_y   = config->mirror_y;
    dev->base.int_io     = (int)config->int_io;

    ESP_LOGI(TAG, "FT6336U ready (chip id 0x%02x)", id);
    *out_touch = &dev->base;
    return ESP_OK;
}
