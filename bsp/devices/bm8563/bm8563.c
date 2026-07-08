/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * BM8563 I2C RTC driver -- see bm8563.h. BCD calendar registers plus the
 * countdown timer (mapped to bsp_rtc's one-shot/interval timer). The chip's
 * timer is periodic in hardware; the driver's INT handler enforces one-shot by
 * disabling the timer on the expiry it services. With no INT gpio wired the
 * timer's INT pin still asserts (on M5Paper it latches the power rail back on);
 * read bsp_rtc_timer_is_expired() and stop it after a deep-sleep wake.
 */

#include "bm8563.h"
#include "bsp_rtc.h"
#include "bsp_dispatch.h"
#include <stdlib.h>
#include <string.h>
#include "esp_check.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

static const char *TAG = "bm8563";

/* ---- Registers ---------------------------------------------------------- */
#define REG_CTRL_STATUS2  0x01
#define REG_SECONDS       0x02   /* 0x02..0x08: seconds..years (BCD)         */
#define REG_TIMER_CTRL    0x0E
#define REG_TIMER_COUNT   0x0F

#define SECONDS_VL        0x80u  /* voltage-low: time invalid                */
#define MONTHS_CENTURY    0x80u  /* 1 -> 19xx, 0 -> 20xx                     */

#define CS2_TI_TP         0x10u  /* INT mode: 0 = level (follows TF)         */
#define CS2_TF            0x04u  /* timer flag                               */
#define CS2_TIE           0x01u  /* timer interrupt (INT pin) enable         */

#define TIMER_ENABLE      0x80u
#define TIMER_SRC_4096HZ  0x00u  /* ~244 us tick -> up to 62 ms              */
#define TIMER_SRC_64HZ    0x01u  /* ~15.6 ms tick -> up to ~3.98 s           */
#define TIMER_SRC_1HZ     0x02u  /* 1 s tick   -> up to 255 s                */
#define TIMER_SRC_1_60HZ  0x03u  /* 1/60 Hz    -> minute granularity         */
#define TIMER_MAX_MS      (255u * 60u * 1000u)

#define I2C_TIMEOUT_MS    50
#define TASK_STACK        2560

typedef struct {
    bsp_rtc_t               base;     /* vtable; must be first                */
    bm8563_config_t         cfg;
    i2c_master_dev_handle_t i2c_dev;
    SemaphoreHandle_t       lock;

    /* INT-line deferred dispatch (only when cfg.int_io is wired). */
    SemaphoreHandle_t       irq;
    TaskHandle_t            task;
    SemaphoreHandle_t       task_done;
    volatile bool           task_stop;
    bool                    repeat;
    bsp_rtc_int_cb_t        int_cb;
    void                   *int_arg;
} bm8563_dev_t;

/* ---- I2C primitives (1-byte register address) --------------------------- */

static esp_err_t reg_read(bm8563_dev_t *dev, uint8_t reg, uint8_t *buf, size_t len) {
    return i2c_master_transmit_receive(dev->i2c_dev, &reg, 1, buf, len, I2C_TIMEOUT_MS);
}

static esp_err_t reg_write(bm8563_dev_t *dev, uint8_t reg, const uint8_t *buf, size_t len) {
    uint8_t scratch[10];
    if (len + 1 > sizeof(scratch)) return ESP_ERR_INVALID_SIZE;
    scratch[0] = reg;
    if (len) memcpy(&scratch[1], buf, len);
    return i2c_master_transmit(dev->i2c_dev, scratch, len + 1, I2C_TIMEOUT_MS);
}

static esp_err_t reg_write_byte(bm8563_dev_t *dev, uint8_t reg, uint8_t value) {
    return reg_write(dev, reg, &value, 1);
}

static inline void dev_lock(bm8563_dev_t *dev)   { xSemaphoreTake(dev->lock, portMAX_DELAY); }
static inline void dev_unlock(bm8563_dev_t *dev) { xSemaphoreGive(dev->lock); }

static inline uint8_t bcd2dec(uint8_t b) { return (uint8_t)((b >> 4) * 10 + (b & 0x0f)); }
static inline uint8_t dec2bcd(uint8_t d) { return (uint8_t)(((d / 10) << 4) | (d % 10)); }

/* ---- Time --------------------------------------------------------------- */

static esp_err_t bm8563_get_time(bsp_rtc_t *self, bsp_rtc_datetime_t *out) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    uint8_t b[7];
    dev_lock(dev);
    esp_err_t err = reg_read(dev, REG_SECONDS, b, sizeof(b));
    dev_unlock(dev);
    if (err != ESP_OK) return err;

    out->second  = bcd2dec(b[0] & 0x7f);
    out->minute  = bcd2dec(b[1] & 0x7f);
    out->hour    = bcd2dec(b[2] & 0x3f);
    out->day     = bcd2dec(b[3] & 0x3f);
    out->weekday = bcd2dec(b[4] & 0x07);
    out->month   = bcd2dec(b[5] & 0x1f);
    out->year    = (uint16_t)((b[5] & MONTHS_CENTURY) ? 1900 : 2000) + bcd2dec(b[6]);
    return ESP_OK;
}

static esp_err_t bm8563_set_time(bsp_rtc_t *self, const bsp_rtc_datetime_t *dt) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    uint8_t b[7];
    b[0] = dec2bcd(dt->second);   /* writing seconds clears the VL flag */
    b[1] = dec2bcd(dt->minute);
    b[2] = dec2bcd(dt->hour);
    b[3] = dec2bcd(dt->day);
    b[4] = dec2bcd(dt->weekday);
    b[5] = dec2bcd(dt->month) | (dt->year < 2000 ? MONTHS_CENTURY : 0);
    b[6] = dec2bcd((uint8_t)(dt->year % 100));

    dev_lock(dev);
    esp_err_t err = reg_write(dev, REG_SECONDS, b, sizeof(b));
    dev_unlock(dev);
    return err;
}

static esp_err_t bm8563_time_is_valid(bsp_rtc_t *self, bool *out_valid) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    uint8_t sec = 0;
    dev_lock(dev);
    esp_err_t err = reg_read(dev, REG_SECONDS, &sec, 1);
    dev_unlock(dev);
    if (err != ESP_OK) return err;
    *out_valid = !(sec & SECONDS_VL);
    return ESP_OK;
}

/* ---- Countdown timer ---------------------------------------------------- */

/* Pick the finest source clock that spans `ms` (count 1..255), rounding to the
 * nearest tick. */
static void pick_source(uint32_t ms, uint8_t *src, uint8_t *count) {
    uint32_t c;
    if (ms <= 62) {            /* 255 / 4096 * 1000 */
        *src = TIMER_SRC_4096HZ;
        c = (ms * 4096 + 500) / 1000;
    } else if (ms <= 3984) {   /* 255 / 64 * 1000 */
        *src = TIMER_SRC_64HZ;
        c = (ms * 64 + 500) / 1000;
    } else if (ms <= 255000) {
        *src = TIMER_SRC_1HZ;
        c = (ms + 500) / 1000;
    } else {
        *src = TIMER_SRC_1_60HZ;
        c = (ms + 30000) / 60000;
    }
    *count = (uint8_t)(c < 1 ? 1 : c > 255 ? 255 : c);
}

static esp_err_t bm8563_timer_start(bsp_rtc_t *self, uint32_t ms, bool repeat) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    if (ms == 0 || ms > TIMER_MAX_MS) return ESP_ERR_INVALID_ARG;

    uint8_t src, count;
    pick_source(ms, &src, &count);

    dev_lock(dev);
    dev->repeat = repeat;
    uint8_t cs2 = 0;
    esp_err_t err = reg_read(dev, REG_CTRL_STATUS2, &cs2, 1);
    if (err == ESP_OK) err = reg_write_byte(dev, REG_TIMER_CTRL, 0);  /* stop while reconfiguring */
    if (err == ESP_OK) {
        cs2 = (uint8_t)((cs2 & ~(CS2_TF | CS2_TI_TP)) | CS2_TIE);     /* clear flag, level INT, enable */
        err = reg_write_byte(dev, REG_CTRL_STATUS2, cs2);
    }
    if (err == ESP_OK) err = reg_write_byte(dev, REG_TIMER_COUNT, count);
    if (err == ESP_OK) err = reg_write_byte(dev, REG_TIMER_CTRL, (uint8_t)(TIMER_ENABLE | src));
    dev_unlock(dev);
    return err;
}

static esp_err_t bm8563_timer_stop(bsp_rtc_t *self) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    dev_lock(dev);
    uint8_t cs2 = 0;
    esp_err_t err = reg_write_byte(dev, REG_TIMER_CTRL, 0);
    if (err == ESP_OK) err = reg_read(dev, REG_CTRL_STATUS2, &cs2, 1);
    if (err == ESP_OK) {
        cs2 &= (uint8_t)~(CS2_TF | CS2_TIE);
        err = reg_write_byte(dev, REG_CTRL_STATUS2, cs2);
    }
    dev_unlock(dev);
    return err;
}

static esp_err_t bm8563_timer_is_expired(bsp_rtc_t *self, bool *out_expired) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    uint8_t cs2 = 0;
    dev_lock(dev);
    esp_err_t err = reg_read(dev, REG_CTRL_STATUS2, &cs2, 1);
    dev_unlock(dev);
    if (err != ESP_OK) return err;
    *out_expired = !!(cs2 & CS2_TF);
    return ESP_OK;
}

static esp_err_t bm8563_timer_clear(bsp_rtc_t *self) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    dev_lock(dev);
    uint8_t cs2 = 0;
    esp_err_t err = reg_read(dev, REG_CTRL_STATUS2, &cs2, 1);
    if (err == ESP_OK) err = reg_write_byte(dev, REG_CTRL_STATUS2, (uint8_t)(cs2 & ~CS2_TF));
    dev_unlock(dev);
    return err;
}

/* ---- INT line ----------------------------------------------------------- */

static void IRAM_ATTR int_isr(void *arg) {
    bm8563_dev_t *dev = arg;
    BaseType_t hp = pdFALSE;
    xSemaphoreGiveFromISR(dev->irq, &hp);
    if (hp) portYIELD_FROM_ISR();
}

/* Deferred dispatch: on the expiry it services, a one-shot disables the timer; an
 * interval just clears the flag to re-arm. Then the user callback runs (off lock). */
static void int_task(void *arg) {
    bm8563_dev_t *dev = arg;
    while (!dev->task_stop) {
        xSemaphoreTake(dev->irq, portMAX_DELAY);
        if (dev->task_stop) break;

        bool fired = false;
        dev_lock(dev);
        uint8_t cs2 = 0;
        if (reg_read(dev, REG_CTRL_STATUS2, &cs2, 1) == ESP_OK && (cs2 & CS2_TF)) {
            fired = true;
            if (dev->repeat) {
                reg_write_byte(dev, REG_CTRL_STATUS2, (uint8_t)(cs2 & ~CS2_TF));
            } else {
                reg_write_byte(dev, REG_TIMER_CTRL, 0);
                reg_write_byte(dev, REG_CTRL_STATUS2, (uint8_t)(cs2 & ~(CS2_TF | CS2_TIE)));
            }
        }
        bsp_rtc_int_cb_t cb = dev->int_cb;
        void *cb_arg = dev->int_arg;
        dev_unlock(dev);

        if (fired && cb) cb(cb_arg);
    }
    xSemaphoreGive(dev->task_done);
    vTaskDelete(NULL);
}

static esp_err_t bm8563_set_int_cb(bsp_rtc_t *self, bsp_rtc_int_cb_t cb, void *arg) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    dev_lock(dev);
    dev->int_cb  = cb;
    dev->int_arg = arg;
    dev_unlock(dev);
    return ESP_OK;
}

/* BM8563 INT idles high and is driven low while a flag is active (level mode). */
static esp_err_t int_setup(bm8563_dev_t *dev) {
    gpio_config_t cfg = {
        .pin_bit_mask = 1ULL << dev->cfg.int_io,
        .mode         = GPIO_MODE_INPUT,
        .pull_up_en   = GPIO_IS_VALID_OUTPUT_GPIO(dev->cfg.int_io) ? GPIO_PULLUP_ENABLE
                                                                   : GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_NEGEDGE,
    };
    ESP_RETURN_ON_ERROR(gpio_config(&cfg), TAG, "int gpio_config");

    dev->irq = xSemaphoreCreateBinary();
    dev->task_done = xSemaphoreCreateBinary();
    if (!dev->irq || !dev->task_done) return ESP_ERR_NO_MEM;
    if (xTaskCreate(int_task, "bm8563", TASK_STACK, dev, 5, &dev->task) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }

    ESP_RETURN_ON_ERROR(bsp_dispatch_install_gpio_isr(), TAG, "isr service");
    ESP_RETURN_ON_ERROR(gpio_isr_handler_add(dev->cfg.int_io, int_isr, dev), TAG, "isr add");
    return ESP_OK;
}

/* ---- Lifecycle ---------------------------------------------------------- */

static esp_err_t bm8563_deinit(bsp_rtc_t *self) {
    bm8563_dev_t *dev = (bm8563_dev_t *)self;
    if (!dev) return ESP_OK;
    if (dev->task) {
        dev->task_stop = true;
        xSemaphoreGive(dev->irq);
        xSemaphoreTake(dev->task_done, portMAX_DELAY);
    }
    if (dev->cfg.int_io != GPIO_NUM_NC) {
        gpio_intr_disable(dev->cfg.int_io);
        gpio_isr_handler_remove(dev->cfg.int_io);
    }
    if (dev->i2c_dev)   i2c_master_bus_rm_device(dev->i2c_dev);
    if (dev->irq)       vSemaphoreDelete(dev->irq);
    if (dev->task_done) vSemaphoreDelete(dev->task_done);
    if (dev->lock)      vSemaphoreDelete(dev->lock);
    free(dev);
    return ESP_OK;
}

esp_err_t bm8563_rtc_create(const bm8563_config_t *config, bsp_rtc_t **out_rtc) {
    ESP_RETURN_ON_FALSE(config && out_rtc, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(config->i2c_bus, ESP_ERR_INVALID_ARG, TAG, "i2c_bus required");

    bm8563_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;
    dev->base.get_time         = bm8563_get_time;
    dev->base.set_time         = bm8563_set_time;
    dev->base.time_is_valid    = bm8563_time_is_valid;
    dev->base.deinit           = bm8563_deinit;
    dev->base.timer_start      = bm8563_timer_start;
    dev->base.timer_stop       = bm8563_timer_stop;
    dev->base.timer_is_expired = bm8563_timer_is_expired;
    dev->base.timer_clear      = bm8563_timer_clear;
    dev->base.set_int_cb       = config->int_io != GPIO_NUM_NC ? bm8563_set_int_cb : NULL;

    dev->lock = xSemaphoreCreateMutex();
    if (!dev->lock) { free(dev); return ESP_ERR_NO_MEM; }

    uint8_t addr = config->i2c_address ? config->i2c_address : BM8563_I2C_ADDR;
    esp_err_t err = i2c_master_probe(config->i2c_bus, addr, I2C_TIMEOUT_MS);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "no BM8563 at 0x%02x: %s", addr, esp_err_to_name(err));
        bm8563_deinit(&dev->base);
        return err;
    }
    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = addr,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : BM8563_I2C_DEFAULT_HZ,
    };
    err = i2c_master_bus_add_device(config->i2c_bus, &dev_cfg, &dev->i2c_dev);
    if (err != ESP_OK) { bm8563_deinit(&dev->base); return err; }

    if (config->int_io != GPIO_NUM_NC) {
        err = int_setup(dev);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "INT setup failed: %s", esp_err_to_name(err));
            bm8563_deinit(&dev->base);
            return err;
        }
    }

    *out_rtc = &dev->base;
    return ESP_OK;
}
