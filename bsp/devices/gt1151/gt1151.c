/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT1151 touch driver -- see gt1151.h. Chip-specific parts only: I2C attach +
 * HW reset (INT-level address latch) + one-shot chip poll. The reader task, INT
 * ISR, orientation transform, and the INT->poll->INT state machine live in the
 * common layer (src/bsp_touch.c); this file's bsp_touch_t::poll just fills raw
 * (chip-space) coords + a fresh flag.
 *
 * Structurally the GT911 driver with HotKnot stripped and the touch-point
 * capacity raised to 10.
 */

#include "gt1151.h"
#include "bsp_touch.h"
#include <stdlib.h>
#include <string.h>
#include "esp_check.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

static const char *TAG = "gt1151";

/* ---- Registers ---------------------------------------------------------- */
#define REG_COMMAND       0x8040u  /* write 0x05 to sleep                    */
#define REG_PRODUCT_ID    0x8140u  /* 4 ASCII bytes                          */
#define REG_STATUS        0x814Eu  /* bit7 = data ready, bits[3:0] = count   */
#define REG_TOUCH_DATA    0x814Fu  /* 8 bytes per point, up to 10 points     */

#define STATUS_READY      0x80u
#define STATUS_COUNT_MASK 0x0Fu
#define POINT_SIZE        8u

/* ---- Timing ------------------------------------------------------------- */
#define RESET_LOW_US      200u    /* datasheet: >100us                       */
#define ADDR_HOLD_MS      5u      /* INT level held after RESET rising edge  */
#define BOOT_MS           50u     /* let firmware come up before I2C         */
#define I2C_TIMEOUT_MS    50

typedef struct {
    uint8_t  track_id;
    uint16_t x;
    uint16_t y;
    uint16_t size;
} gt1151_point_t;

struct gt1151_dev {
    bsp_touch_t             base;     /* vtable + base fields; must be first  */
    gt1151_config_t         cfg;
    i2c_master_dev_handle_t i2c_dev;
    uint8_t                 i2c_address;
    bool                    int_is_input;
    SemaphoreHandle_t       lock;

    gt1151_point_t          points[GT1151_MAX_TOUCH_POINTS];
    uint8_t                 count;
    bool                    fresh;
};

/* ========================================================================
 * I2C primitives -- GT1151 uses 16-bit big-endian register addresses.
 * ===================================================================== */

static esp_err_t reg_read_locked(struct gt1151_dev *dev, uint16_t reg, uint8_t *buf, size_t len) {
    uint8_t addr[2] = { (uint8_t)(reg >> 8), (uint8_t)reg };
    return i2c_master_transmit_receive(dev->i2c_dev, addr, 2, buf, len, I2C_TIMEOUT_MS);
}

static esp_err_t reg_write_locked(struct gt1151_dev *dev, uint16_t reg, const uint8_t *buf, size_t len) {
    uint8_t scratch[16];
    if (len + 2 > sizeof(scratch)) return ESP_ERR_INVALID_SIZE;
    scratch[0] = (uint8_t)(reg >> 8);
    scratch[1] = (uint8_t)reg;
    if (len) memcpy(&scratch[2], buf, len);
    return i2c_master_transmit(dev->i2c_dev, scratch, len + 2, I2C_TIMEOUT_MS);
}

static inline esp_err_t reg_write_byte_locked(struct gt1151_dev *dev, uint16_t reg, uint8_t value) {
    return reg_write_locked(dev, reg, &value, 1);
}

static inline void dev_lock(struct gt1151_dev *dev)   { if (dev->lock) xSemaphoreTake(dev->lock, portMAX_DELAY); }
static inline void dev_unlock(struct gt1151_dev *dev) { if (dev->lock) xSemaphoreGive(dev->lock); }

/* ========================================================================
 * GPIO / reset
 * ===================================================================== */

static esp_err_t gpio_as_output(gpio_num_t pin, int level) {
    gpio_config_t cfg = {
        .pin_bit_mask = 1ULL << pin,
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&cfg);
    if (err == ESP_OK) gpio_set_level(pin, level);
    return err;
}

static esp_err_t gpio_as_input(gpio_num_t pin, bool pullup) {
    if (pullup && !GPIO_IS_VALID_OUTPUT_GPIO(pin)) pullup = false;
    gpio_config_t cfg = {
        .pin_bit_mask = 1ULL << pin,
        .mode         = GPIO_MODE_INPUT,
        .pull_up_en   = pullup ? GPIO_PULLUP_ENABLE : GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    return gpio_config(&cfg);
}

/* Latches its I2C address from INT level at the RESET rising edge:
 *   INT high -> 0x14, INT low -> 0x5D.
 * When RESET is not wired we only re-assert INT as an input (the chip is not
 * actually reset -- a power cycle is the only way to change the latched addr). */
static esp_err_t hw_reset(struct gt1151_dev *dev, uint8_t target_addr) {
    const bool has_rst   = dev->cfg.reset_io != GPIO_NUM_NC;
    const bool has_int   = dev->cfg.int_io   != GPIO_NUM_NC;
    const bool drive_int = has_int && GPIO_IS_VALID_OUTPUT_GPIO(dev->cfg.int_io);

    if (!has_rst) {
        if (has_int) (void)gpio_as_input(dev->cfg.int_io, true);
        dev->int_is_input = has_int;
        return ESP_OK;
    }

    esp_err_t err = gpio_as_output(dev->cfg.reset_io, 0);
    if (err != ESP_OK) return err;

    if (drive_int) {
        int level = (target_addr == GT1151_I2C_ADDR_SECONDARY) ? 0 : 1;
        err = gpio_as_output(dev->cfg.int_io, level);
        if (err != ESP_OK) return err;
    }
    esp_rom_delay_us(RESET_LOW_US);

    gpio_set_level(dev->cfg.reset_io, 1);
    vTaskDelay(pdMS_TO_TICKS(ADDR_HOLD_MS));

    if (has_int) {
        err = gpio_as_input(dev->cfg.int_io, true);
        if (err != ESP_OK) return err;
        dev->int_is_input = true;
    }
    vTaskDelay(pdMS_TO_TICKS(BOOT_MS));
    return ESP_OK;
}

/* ========================================================================
 * Address probe + info
 * ===================================================================== */

static esp_err_t attach_device(struct gt1151_dev *dev, uint8_t addr) {
    if (dev->i2c_dev) {
        i2c_master_bus_rm_device(dev->i2c_dev);
        dev->i2c_dev = NULL;
    }
    i2c_device_config_t cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = addr,
        .scl_speed_hz    = dev->cfg.clock_hz > 0 ? dev->cfg.clock_hz : GT1151_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(dev->cfg.i2c_bus, &cfg, &dev->i2c_dev);
    if (err == ESP_OK) dev->i2c_address = addr;
    return err;
}

static esp_err_t probe_and_attach(struct gt1151_dev *dev, uint8_t hint) {
    uint8_t candidates[2];
    size_t n = 0;
    if (hint == GT1151_I2C_ADDR_PRIMARY || hint == GT1151_I2C_ADDR_SECONDARY) {
        candidates[n++] = hint;
        candidates[n++] = (hint == GT1151_I2C_ADDR_PRIMARY)
                              ? GT1151_I2C_ADDR_SECONDARY : GT1151_I2C_ADDR_PRIMARY;
    } else {
        candidates[n++] = GT1151_I2C_ADDR_PRIMARY;
        candidates[n++] = GT1151_I2C_ADDR_SECONDARY;
    }
    for (size_t i = 0; i < n; i++) {
        if (i2c_master_probe(dev->cfg.i2c_bus, candidates[i], I2C_TIMEOUT_MS) == ESP_OK) {
            return attach_device(dev, candidates[i]);
        }
    }
    return ESP_ERR_NOT_FOUND;
}

static void log_info_locked(struct gt1151_dev *dev) {
    uint8_t buf[11];
    if (reg_read_locked(dev, REG_PRODUCT_ID, buf, sizeof(buf)) != ESP_OK) return;
    char id[5] = { (char)buf[0], (char)buf[1], (char)buf[2], (char)buf[3], '\0' };
    uint16_t fw  = (uint16_t)buf[4] | ((uint16_t)buf[5] << 8);
    uint16_t xr  = (uint16_t)buf[6] | ((uint16_t)buf[7] << 8);
    uint16_t yr  = (uint16_t)buf[8] | ((uint16_t)buf[9] << 8);
    ESP_LOGI(TAG, "id=%s fw=0x%04x res=%ux%u vendor=0x%02x @0x%02x",
             id, fw, xr, yr, buf[10], dev->i2c_address);
}

/* ========================================================================
 * Coord poll -- reads 0x814E; on a fresh sample, reads the point list and
 * clears the chip's data-ready latch. Idempotent. Caller holds dev->lock.
 * ===================================================================== */
static esp_err_t coord_poll_locked(struct gt1151_dev *dev) {
    dev->fresh = false;

    uint8_t status = 0;
    esp_err_t err = reg_read_locked(dev, REG_STATUS, &status, 1);
    if (err != ESP_OK) return err;
    if (!(status & STATUS_READY)) return ESP_OK;

    uint8_t n = status & STATUS_COUNT_MASK;
    if (n > GT1151_MAX_TOUCH_POINTS) n = GT1151_MAX_TOUCH_POINTS;

    dev->count = 0;
    dev->fresh = true;
    if (n > 0) {
        uint8_t raw[GT1151_MAX_TOUCH_POINTS * POINT_SIZE];
        err = reg_read_locked(dev, REG_TOUCH_DATA, raw, (size_t)n * POINT_SIZE);
        if (err != ESP_OK) {
            /* Always clear the latch so a transient glitch doesn't wedge us
             * reporting stale data forever. */
            (void)reg_write_byte_locked(dev, REG_STATUS, 0);
            return err;
        }
        for (uint8_t i = 0; i < n; i++) {
            const uint8_t *p = &raw[i * POINT_SIZE];
            gt1151_point_t *out = &dev->points[dev->count++];
            out->track_id = p[0];
            out->x        = (uint16_t)p[1] | ((uint16_t)p[2] << 8);
            out->y        = (uint16_t)p[3] | ((uint16_t)p[4] << 8);
            out->size     = (uint16_t)p[5] | ((uint16_t)p[6] << 8);
        }
    }

    return reg_write_byte_locked(dev, REG_STATUS, 0);
}

/* ========================================================================
 * bsp_touch_t vtable
 * ===================================================================== */

static esp_err_t gt1151_poll(bsp_touch_t *self,
                             bsp_touch_raw_point_t *out, uint8_t max,
                             uint8_t *count, bool *keep_polling) {
    struct gt1151_dev *dev = (struct gt1151_dev *)self;

    dev_lock(dev);
    esp_err_t err = coord_poll_locked(dev);
    uint8_t n = 0;
    if (err == ESP_OK) {
        n = dev->count;
        if (n > max) n = max;
        for (uint8_t i = 0; i < n; i++) {
            out[i].x  = dev->points[i].x;
            out[i].y  = dev->points[i].y;
            out[i].id = dev->points[i].track_id;
        }
    }
    dev_unlock(dev);

    *count = n;
    *keep_polling = false;
    return err;
}

static esp_err_t gt1151_deinit(bsp_touch_t *self) {
    struct gt1151_dev *dev = (struct gt1151_dev *)self;
    if (!dev) return ESP_OK;
    if (dev->i2c_dev) i2c_master_bus_rm_device(dev->i2c_dev);
    if (dev->lock)    vSemaphoreDelete(dev->lock);
    free(dev);
    return ESP_OK;
}

/* ========================================================================
 * Lifecycle
 * ===================================================================== */

esp_err_t gt1151_touch_create(const gt1151_config_t *config, bsp_touch_t **out_touch) {
    ESP_RETURN_ON_FALSE(config && out_touch, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(config->i2c_bus, ESP_ERR_INVALID_ARG, TAG, "i2c_bus required");

    struct gt1151_dev *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;

    /* Chip vtable */
    dev->base.poll   = gt1151_poll;
    dev->base.deinit = gt1151_deinit;

    /* Base fields */
    dev->base.width      = config->width;
    dev->base.height     = config->height;
    dev->base.max_points = GT1151_MAX_TOUCH_POINTS;
    dev->base.swap_xy    = config->swap_xy;
    dev->base.mirror_x   = config->mirror_x;
    dev->base.mirror_y   = config->mirror_y;
    dev->base.int_io     = (int)config->int_io;

    dev->lock = xSemaphoreCreateMutex();
    if (!dev->lock) { free(dev); return ESP_ERR_NO_MEM; }

    esp_err_t err = hw_reset(dev, config->i2c_address);
    if (err == ESP_OK) err = probe_and_attach(dev, config->i2c_address);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "no GT1151 on bus: %s", esp_err_to_name(err));
        gt1151_deinit(&dev->base);
        return err;
    }

    dev_lock(dev);
    log_info_locked(dev);
    dev_unlock(dev);

    *out_touch = &dev->base;
    return ESP_OK;
}
