/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ST7123 I2C touch driver — see st7123_touch.h. Polling reads over I2C with
 * an optional INT fast path and an optional background reader task that pushes
 * samples to bsp_touch_emit_event(). Trimmed version of the GT911 driver (no
 * HotKnot), with the ST7123's own 7-byte report layout.
 */

#include "st7123_touch.h"
#include "bsp_touch.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

static const char *TAG = "st7123_tp";

/* Registers (16-bit big-endian addresses) ---------------------------------- */
#define REG_ADV_INFO       0x0010u   /* bit 3 = with_coord, bit 7 = chip_reset */
#define REG_MAX_TOUCHES    0x0009u   /* max touches available this frame       */
#define REG_REPORT_COORD   0x0014u   /* start of touch reports                 */

#define ADV_INFO_WITH_COORD 0x08u    /* 1 << 3                                 */
#define POINT_SIZE          7u       /* bytes per touch report                 */

/* Timing ------------------------------------------------------------------- */
#define RESET_LOW_MS       10u
#define BOOT_MS            10u
#define I2C_TIMEOUT_MS     50
#define POLL_INTERVAL_MS   10
#define TASK_STACK_DEFAULT 3072

typedef struct {
    uint16_t x, y;
    uint8_t  area;     /* per-finger "strength"; not exposed via bsp_touch_t */
} st7123_point_t;

struct st7123_dev {
    bsp_touch_t             base;
    st7123_touch_config_t   cfg;
    i2c_master_dev_handle_t i2c_dev;
    bool                    int_is_input;
    SemaphoreHandle_t       lock;
    SemaphoreHandle_t       irq;       /* given by INT ISR; NULL if no INT */

    TaskHandle_t      task;
    SemaphoreHandle_t task_done;
    volatile bool     task_stop;
    bool              has_task;

    st7123_point_t          points[ST7123_MAX_TOUCH_POINTS];
    uint8_t                 count;
    bool                    fresh;
};

/* --- I2C primitives (caller holds dev->lock) ------------------------------ */

static esp_err_t reg_read_locked(struct st7123_dev *dev, uint16_t reg, uint8_t *buf, size_t len) {
    uint8_t addr[2] = { (uint8_t)(reg >> 8), (uint8_t)reg };
    return i2c_master_transmit_receive(dev->i2c_dev, addr, 2, buf, len, I2C_TIMEOUT_MS);
}

static inline void dev_lock(struct st7123_dev *dev)   { if (dev->lock) xSemaphoreTake(dev->lock, portMAX_DELAY); }
static inline void dev_unlock(struct st7123_dev *dev) { if (dev->lock) xSemaphoreGive(dev->lock); }

/* --- GPIO / reset / INT --------------------------------------------------- */

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

static void IRAM_ATTR int_isr(void *arg) {
    struct st7123_dev *dev = arg;
    BaseType_t hp = pdFALSE;
    if (dev->irq) xSemaphoreGiveFromISR(dev->irq, &hp);
    if (hp) portYIELD_FROM_ISR();
}

static void int_enable_irq(struct st7123_dev *dev) {
    if (dev->cfg.int_io == GPIO_NUM_NC || !dev->int_is_input || !dev->irq) return;
    esp_err_t err = gpio_install_isr_service(0);
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) {
        ESP_LOGW(TAG, "gpio_install_isr_service: %s", esp_err_to_name(err));
        return;
    }
    gpio_set_intr_type(dev->cfg.int_io, GPIO_INTR_NEGEDGE);
    if (gpio_isr_handler_add(dev->cfg.int_io, int_isr, dev) == ESP_OK) {
        gpio_intr_enable(dev->cfg.int_io);
    }
}

static esp_err_t hw_reset(struct st7123_dev *dev) {
    const bool has_rst = dev->cfg.reset_io != GPIO_NUM_NC;
    const bool has_int = dev->cfg.int_io   != GPIO_NUM_NC;

    if (!has_rst) {
        if (has_int) (void)gpio_as_input(dev->cfg.int_io, true);
        dev->int_is_input = has_int;
        return ESP_OK;
    }
    esp_err_t err = gpio_as_output(dev->cfg.reset_io, 0);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(RESET_LOW_MS));
    gpio_set_level(dev->cfg.reset_io, 1);
    vTaskDelay(pdMS_TO_TICKS(BOOT_MS));

    if (has_int) {
        err = gpio_as_input(dev->cfg.int_io, true);
        if (err != ESP_OK) return err;
        dev->int_is_input = true;
    }
    return ESP_OK;
}

/* --- Coordinate poll (caller holds dev->lock) ----------------------------- */

static esp_err_t coord_poll_locked(struct st7123_dev *dev) {
    dev->fresh = false;

    uint8_t adv = 0;
    esp_err_t err = reg_read_locked(dev, REG_ADV_INFO, &adv, 1);
    if (err != ESP_OK) return err;
    if (!(adv & ADV_INFO_WITH_COORD)) return ESP_OK;

    uint8_t max_touches = 0;
    err = reg_read_locked(dev, REG_MAX_TOUCHES, &max_touches, 1);
    if (err != ESP_OK) return err;
    if (max_touches > ST7123_MAX_TOUCH_POINTS) max_touches = ST7123_MAX_TOUCH_POINTS;

    if (max_touches == 0) {
        dev->count = 0;
        dev->fresh = true;
        return ESP_OK;
    }

    uint8_t buf[ST7123_MAX_TOUCH_POINTS * POINT_SIZE];
    err = reg_read_locked(dev, REG_REPORT_COORD, buf, (size_t)max_touches * POINT_SIZE);
    if (err != ESP_OK) return err;

    uint8_t n = 0;
    for (uint8_t i = 0; i < max_touches; i++) {
        const uint8_t *r = buf + (size_t)i * POINT_SIZE;
        const bool valid = (r[0] & 0x80) != 0;
        if (!valid) continue;
        dev->points[n].x    = ((uint16_t)(r[0] & 0x3F) << 8) | r[1];
        dev->points[n].y    = ((uint16_t)r[2] << 8) | r[3];
        dev->points[n].area = r[4];
        n++;
    }
    dev->count = n;
    dev->fresh = true;
    return ESP_OK;
}

/* --- Orientation mapping (raw chip -> display space) ---------------------- */

static void apply_orientation(const struct st7123_dev *dev,
                              const st7123_point_t *in, bsp_touch_point_t *out, int n) {
    const uint16_t w = dev->cfg.width  ? dev->cfg.width  : 1;
    const uint16_t h = dev->cfg.height ? dev->cfg.height : 1;
    for (int i = 0; i < n; i++) {
        int x = in[i].x;
        int y = in[i].y;
        if (dev->cfg.swap_xy)   { int t = x; x = y; y = t; }
        if (dev->cfg.mirror_x)  x = (int)w - 1 - x;
        if (dev->cfg.mirror_y)  y = (int)h - 1 - y;
        out[i].x  = x;
        out[i].y  = y;
        out[i].id = i;          /* ST7123 reports don't carry a track id */
    }
}

/* --- Reader task ---------------------------------------------------------- */

static void reader_task(void *arg) {
    struct st7123_dev *dev = arg;
    const uint32_t poll_ms = dev->cfg.acquire.poll_interval_ms
        ? dev->cfg.acquire.poll_interval_ms : POLL_INTERVAL_MS;

    bool was_down = false;
    while (!dev->task_stop) {
        if (dev->irq) {
            xSemaphoreTake(dev->irq, pdMS_TO_TICKS(poll_ms));
        } else {
            vTaskDelay(pdMS_TO_TICKS(poll_ms));
        }
        if (dev->task_stop) break;

        bsp_touch_point_t out[ST7123_MAX_TOUCH_POINTS];
        int n = 0;

        dev_lock(dev);
        esp_err_t err = coord_poll_locked(dev);
        if (err == ESP_OK && dev->fresh) {
            n = dev->count;
            if (n > 0) apply_orientation(dev, dev->points, out, n);
        }
        dev_unlock(dev);

        if (err != ESP_OK) continue;
        if (n > 0 || was_down) bsp_touch_emit_event(out, n);
        was_down = (n > 0);
    }
    xSemaphoreGive(dev->task_done);
    vTaskDelete(NULL);
}

/* --- bsp_touch vtable ----------------------------------------------------- */

static int touch_read(bsp_touch_t *self, bsp_touch_point_t *points, uint8_t max_points) {
    struct st7123_dev *dev = (struct st7123_dev *)self;
    if (max_points == 0) return 0;

    int n = 0;
    dev_lock(dev);
    /* When a reader task owns the chip, return the cached snapshot to avoid
     * racing it. Without a task, poll synchronously. */
    if (!dev->has_task) {
        if (coord_poll_locked(dev) != ESP_OK) {
            dev_unlock(dev);
            return 0;
        }
    }
    int avail = dev->count;
    if (avail > max_points) avail = max_points;
    if (avail > 0) apply_orientation(dev, dev->points, points, avail);
    n = avail;
    dev_unlock(dev);
    return n;
}

static void touch_wait_interrupt(bsp_touch_t *self) {
    struct st7123_dev *dev = (struct st7123_dev *)self;
    if (dev->irq) {
        xSemaphoreTake(dev->irq, portMAX_DELAY);
    } else {
        vTaskDelay(pdMS_TO_TICKS(POLL_INTERVAL_MS));
    }
}

static esp_err_t touch_deinit(bsp_touch_t *self) {
    struct st7123_dev *dev = (struct st7123_dev *)self;
    if (dev->task) {
        dev->task_stop = true;
        if (dev->irq) xSemaphoreGive(dev->irq);
        if (dev->task_done) xSemaphoreTake(dev->task_done, pdMS_TO_TICKS(500));
    }
    if (dev->cfg.int_io != GPIO_NUM_NC && dev->int_is_input) {
        gpio_intr_disable(dev->cfg.int_io);
        gpio_isr_handler_remove(dev->cfg.int_io);
    }
    if (dev->i2c_dev)   i2c_master_bus_rm_device(dev->i2c_dev);
    if (dev->task_done) vSemaphoreDelete(dev->task_done);
    if (dev->irq)       vSemaphoreDelete(dev->irq);
    if (dev->lock)      vSemaphoreDelete(dev->lock);
    free(dev);
    return ESP_OK;
}

/* --- Lifecycle ------------------------------------------------------------ */

esp_err_t st7123_touch_create(const st7123_touch_config_t *config, bsp_touch_t **out_touch) {
    if (!config || !config->i2c_bus || !out_touch) return ESP_ERR_INVALID_ARG;

    struct st7123_dev *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;
    dev->base.read           = touch_read;
    dev->base.wait_interrupt = touch_wait_interrupt;
    dev->base.deinit         = touch_deinit;
    dev->lock = xSemaphoreCreateMutex();
    if (!dev->lock) { free(dev); return ESP_ERR_NO_MEM; }

    esp_err_t err = hw_reset(dev);
    if (err != ESP_OK) goto fail;

    i2c_device_config_t i2c_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = ST7123_I2C_ADDR,
        .scl_speed_hz    = dev->cfg.clock_hz > 0 ? dev->cfg.clock_hz : ST7123_I2C_DEFAULT_HZ,
    };
    err = i2c_master_bus_add_device(dev->cfg.i2c_bus, &i2c_cfg, &dev->i2c_dev);
    if (err != ESP_OK) goto fail;

    if (dev->cfg.int_io != GPIO_NUM_NC) {
        dev->irq = xSemaphoreCreateBinary();
        if (dev->irq) int_enable_irq(dev);
    }

    if (dev->cfg.acquire.task_priority > 0) {
        dev->task_done = xSemaphoreCreateBinary();
        if (!dev->task_done) { err = ESP_ERR_NO_MEM; goto fail; }
        dev->has_task = true;
        const uint32_t stack = dev->cfg.acquire.task_stack
            ? dev->cfg.acquire.task_stack : TASK_STACK_DEFAULT;
        BaseType_t r;
        if (dev->cfg.acquire.task_affinity >= 0) {
            r = xTaskCreatePinnedToCore(reader_task, "st7123", stack, dev,
                                        dev->cfg.acquire.task_priority, &dev->task,
                                        dev->cfg.acquire.task_affinity);
        } else {
            r = xTaskCreate(reader_task, "st7123", stack, dev,
                            dev->cfg.acquire.task_priority, &dev->task);
        }
        if (r != pdPASS) { err = ESP_FAIL; goto fail; }
    }

    *out_touch = &dev->base;
    return ESP_OK;

fail:
    touch_deinit(&dev->base);
    return err;
}
