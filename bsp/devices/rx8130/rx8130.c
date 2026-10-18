/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * RX8130CE I2C RTC driver -- see rx8130.h. The wakeup timer is periodic in
 * hardware; one-shot is enforced by the INT handler, so with no INT gpio wired
 * stop it with bsp_rtc_timer_stop() after a wake.
 */

#include "rx8130.h"
#include "bsp_rtc.h"
#include "bsp_dispatch.h"
#include <stdlib.h>
#include <string.h>
#include "esp_check.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

static const char *TAG = "rx8130";

#define REG_SECONDS       0x10   /* 0x10..0x16: seconds..years (BCD)         */
#define REG_TIMER_COUNT0  0x1A
#define REG_EXTENSION     0x1C
#define REG_FLAG          0x1D
#define REG_CTRL0         0x1E
#define REG_CTRL1         0x1F
#define REG_DIGITAL_OFFSET 0x30

#define EXT_TE            0x10u
#define EXT_TSEL_MASK     0x07u
#define TSEL_64HZ         0x01u
#define TSEL_1HZ          0x02u
#define TSEL_1_60HZ       0x03u
#define TSEL_1_3600HZ     0x04u

/* Flags are write-0-to-clear and writing 1 is ignored, so a single write clears
 * one flag without the read-modify-write race against a newly raised one. */
#define FLAG_TF           0x10u
#define FLAG_VLF          0x02u
#define FLAG_WRITE_KEEP   0xBFu  /* bit 6 must be written 0                  */

#define CTRL0_TEST        0x80u
#define CTRL0_STOP        0x40u
#define CTRL0_TIE         0x10u

#define CTRL1_CHGEN       0x20u
#define CTRL1_INIEN       0x10u

#define I2C_TIMEOUT_MS    50
#define TASK_STACK        2560

typedef struct {
    bsp_rtc_t               base;     /* vtable; must be first                */
    rx8130_config_t         cfg;
    i2c_master_dev_handle_t i2c_dev;
    SemaphoreHandle_t       lock;

    SemaphoreHandle_t       irq;
    TaskHandle_t            task;
    SemaphoreHandle_t       task_done;
    volatile bool           task_stop;
    bool                    repeat;
    bsp_rtc_int_cb_t        int_cb;
    void                   *int_arg;
} rx8130_dev_t;

static esp_err_t reg_read(rx8130_dev_t *dev, uint8_t reg, uint8_t *buf, size_t len) {
    return i2c_master_transmit_receive(dev->i2c_dev, &reg, 1, buf, len, I2C_TIMEOUT_MS);
}

static esp_err_t reg_write(rx8130_dev_t *dev, uint8_t reg, const uint8_t *buf, size_t len) {
    uint8_t scratch[10];
    if (len + 1 > sizeof(scratch)) return ESP_ERR_INVALID_SIZE;
    scratch[0] = reg;
    if (len) memcpy(&scratch[1], buf, len);
    return i2c_master_transmit(dev->i2c_dev, scratch, len + 1, I2C_TIMEOUT_MS);
}

static esp_err_t reg_write_byte(rx8130_dev_t *dev, uint8_t reg, uint8_t value) {
    return reg_write(dev, reg, &value, 1);
}

static esp_err_t reg_update(rx8130_dev_t *dev, uint8_t reg, uint8_t clear, uint8_t set) {
    uint8_t v = 0;
    esp_err_t err = reg_read(dev, reg, &v, 1);
    if (err != ESP_OK) return err;
    return reg_write_byte(dev, reg, (uint8_t)((v & ~clear) | set));
}

static esp_err_t flag_clear(rx8130_dev_t *dev, uint8_t flag) {
    return reg_write_byte(dev, REG_FLAG, (uint8_t)(FLAG_WRITE_KEEP & ~flag));
}

static inline void dev_lock(rx8130_dev_t *dev)   { xSemaphoreTake(dev->lock, portMAX_DELAY); }
static inline void dev_unlock(rx8130_dev_t *dev) { xSemaphoreGive(dev->lock); }

static inline uint8_t bcd2dec(uint8_t b) { return (uint8_t)((b >> 4) * 10 + (b & 0x0f)); }
static inline uint8_t dec2bcd(uint8_t d) { return (uint8_t)(((d / 10) << 4) | (d % 10)); }

/* ---- Time --------------------------------------------------------------- */

static esp_err_t rx8130_get_time(bsp_rtc_t *self, bsp_rtc_datetime_t *out) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    uint8_t b[7];
    dev_lock(dev);
    esp_err_t err = reg_read(dev, REG_SECONDS, b, sizeof(b));
    dev_unlock(dev);
    if (err != ESP_OK) return err;

    uint8_t week = b[3] & 0x7f;
    out->second  = bcd2dec(b[0] & 0x7f);
    out->minute  = bcd2dec(b[1] & 0x7f);
    out->hour    = bcd2dec(b[2] & 0x3f);
    out->weekday = week ? (uint8_t)__builtin_ctz(week) : 0;
    out->day     = bcd2dec(b[4] & 0x3f);
    out->month   = bcd2dec(b[5] & 0x1f);
    out->year    = (uint16_t)(2000 + bcd2dec(b[6]));
    return ESP_OK;
}

static esp_err_t rx8130_set_time(bsp_rtc_t *self, const bsp_rtc_datetime_t *dt) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    if (dt->year < 2000 || dt->year > 2099 || dt->weekday > 6) return ESP_ERR_INVALID_ARG;
    uint8_t b[7];
    b[0] = dec2bcd(dt->second);
    b[1] = dec2bcd(dt->minute);
    b[2] = dec2bcd(dt->hour);
    b[3] = (uint8_t)(1u << dt->weekday);
    b[4] = dec2bcd(dt->day);
    b[5] = dec2bcd(dt->month);
    b[6] = dec2bcd((uint8_t)(dt->year - 2000));

    dev_lock(dev);
    esp_err_t err = reg_write(dev, REG_SECONDS, b, sizeof(b));
    /* VLF stays set until the oscillator runs; time_is_valid keeps reporting it. */
    if (err == ESP_OK) err = flag_clear(dev, FLAG_VLF);
    dev_unlock(dev);
    return err;
}

static esp_err_t rx8130_time_is_valid(bsp_rtc_t *self, bool *out_valid) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    uint8_t flag = 0;
    dev_lock(dev);
    esp_err_t err = reg_read(dev, REG_FLAG, &flag, 1);
    dev_unlock(dev);
    if (err != ESP_OK) return err;
    *out_valid = !(flag & FLAG_VLF);
    return ESP_OK;
}

/* ---- Wakeup timer ------------------------------------------------------- */

/* Finest source clock whose 16-bit count spans `ms`, rounded to the nearest tick.
 * 4096 Hz is never used: its /IRQ pulse is only 122 us against 7.57 ms for the
 * others, too short to latch a board power rail. */
static void pick_source(uint32_t ms, uint8_t *tsel, uint16_t *count) {
    uint32_t c;
    if (ms <= 1023984) {         /* 65535 / 64 * 1000 */
        *tsel = TSEL_64HZ;
        c = (ms * 64 + 500) / 1000;
    } else if (ms <= 65535000) {
        *tsel = TSEL_1HZ;
        c = ms / 1000 + (ms % 1000 >= 500);
    } else if (ms <= 3932100000u) {
        *tsel = TSEL_1_60HZ;
        c = ms / 60000 + (ms % 60000 >= 30000);
    } else {
        *tsel = TSEL_1_3600HZ;
        c = ms / 3600000 + (ms % 3600000 >= 1800000);
    }
    *count = (uint16_t)(c < 1 ? 1 : c > 65535 ? 65535 : c);
}

static esp_err_t timer_disable(rx8130_dev_t *dev) {
    esp_err_t err = reg_update(dev, REG_EXTENSION, EXT_TE, 0);
    if (err == ESP_OK) err = reg_update(dev, REG_CTRL0, CTRL0_TEST | CTRL0_TIE, 0);
    if (err == ESP_OK) err = flag_clear(dev, FLAG_TF);
    return err;
}

static esp_err_t rx8130_timer_start(bsp_rtc_t *self, uint32_t ms, bool repeat) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    if (ms == 0) return ESP_ERR_INVALID_ARG;
    if (ms < dev->cfg.timer_min_ms) return ESP_ERR_NOT_SUPPORTED;

    uint8_t tsel;
    uint16_t count;
    pick_source(ms, &tsel, &count);
    const uint8_t preset[2] = { (uint8_t)count, (uint8_t)(count >> 8) };

    dev_lock(dev);
    dev->repeat = repeat;
    uint8_t ext = 0;
    esp_err_t err = reg_read(dev, REG_EXTENSION, &ext, 1);
    if (err == ESP_OK) {
        ext = (uint8_t)((ext & ~(EXT_TE | EXT_TSEL_MASK)) | tsel);
        err = reg_write_byte(dev, REG_EXTENSION, ext);
    }
    if (err == ESP_OK) err = flag_clear(dev, FLAG_TF);
    if (err == ESP_OK) err = reg_update(dev, REG_CTRL0, CTRL0_TEST, CTRL0_TIE);
    if (err == ESP_OK) err = reg_write(dev, REG_TIMER_COUNT0, preset, sizeof(preset));
    if (err == ESP_OK) err = reg_write_byte(dev, REG_EXTENSION, (uint8_t)(ext | EXT_TE));
    dev_unlock(dev);
    return err;
}

static esp_err_t rx8130_timer_stop(bsp_rtc_t *self) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    dev_lock(dev);
    esp_err_t err = timer_disable(dev);
    dev_unlock(dev);
    return err;
}

static esp_err_t rx8130_timer_is_expired(bsp_rtc_t *self, bool *out_expired) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    uint8_t flag = 0;
    dev_lock(dev);
    esp_err_t err = reg_read(dev, REG_FLAG, &flag, 1);
    dev_unlock(dev);
    if (err != ESP_OK) return err;
    *out_expired = !!(flag & FLAG_TF);
    return ESP_OK;
}

static esp_err_t rx8130_timer_clear(bsp_rtc_t *self) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    dev_lock(dev);
    esp_err_t err = flag_clear(dev, FLAG_TF);
    dev_unlock(dev);
    return err;
}

/* ---- INT line ----------------------------------------------------------- */

static void IRAM_ATTR int_isr(void *arg) {
    rx8130_dev_t *dev = arg;
    BaseType_t hp = pdFALSE;
    xSemaphoreGiveFromISR(dev->irq, &hp);
    if (hp) portYIELD_FROM_ISR();
}

static void int_task(void *arg) {
    rx8130_dev_t *dev = arg;
    while (!dev->task_stop) {
        xSemaphoreTake(dev->irq, portMAX_DELAY);
        if (dev->task_stop) break;

        bool fired = false;
        dev_lock(dev);
        uint8_t flag = 0;
        if (reg_read(dev, REG_FLAG, &flag, 1) == ESP_OK && (flag & FLAG_TF)) {
            fired = true;
            if (dev->repeat) {
                flag_clear(dev, FLAG_TF);
            } else {
                timer_disable(dev);
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

static esp_err_t rx8130_set_int_cb(bsp_rtc_t *self, bsp_rtc_int_cb_t cb, void *arg) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
    dev_lock(dev);
    dev->int_cb  = cb;
    dev->int_arg = arg;
    dev_unlock(dev);
    return ESP_OK;
}

static esp_err_t int_setup(rx8130_dev_t *dev) {
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
    if (xTaskCreate(int_task, "rx8130", TASK_STACK, dev, 5, &dev->task) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }

    ESP_RETURN_ON_ERROR(bsp_dispatch_install_gpio_isr(), TAG, "isr service");
    ESP_RETURN_ON_ERROR(gpio_isr_handler_add(dev->cfg.int_io, int_isr, dev), TAG, "isr add");
    return ESP_OK;
}

/* ---- Lifecycle ---------------------------------------------------------- */

/* VLF is left set so time_is_valid reports the loss until set_time. */
static esp_err_t chip_init(rx8130_dev_t *dev) {
    uint8_t flag = 0;
    esp_err_t err = reg_read(dev, REG_FLAG, &flag, 1);
    if (err == ESP_OK && (flag & FLAG_VLF)) {
        ESP_LOGW(TAG, "oscillation stop detected, time invalid");
        err = reg_write_byte(dev, REG_DIGITAL_OFFSET, 0);
        if (err == ESP_OK) err = reg_update(dev, REG_EXTENSION, EXT_TE, 0);
        if (err == ESP_OK) err = reg_write_byte(dev, REG_CTRL0, 0);
    }
    if (err == ESP_OK) {
        err = reg_update(dev, REG_CTRL0, CTRL0_TEST | CTRL0_STOP, 0);
    }
    if (err == ESP_OK) {
        err = reg_update(dev, REG_CTRL1, CTRL1_CHGEN | CTRL1_INIEN,
                         (uint8_t)(CTRL1_INIEN | (dev->cfg.charge_backup ? CTRL1_CHGEN : 0)));
    }
    return err;
}

static esp_err_t rx8130_deinit(bsp_rtc_t *self) {
    rx8130_dev_t *dev = (rx8130_dev_t *)self;
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

esp_err_t rx8130_rtc_create(const rx8130_config_t *config, bsp_rtc_t **out_rtc) {
    ESP_RETURN_ON_FALSE(config && out_rtc, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(config->i2c_bus, ESP_ERR_INVALID_ARG, TAG, "i2c_bus required");

    rx8130_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;
    dev->base.get_time         = rx8130_get_time;
    dev->base.set_time         = rx8130_set_time;
    dev->base.time_is_valid    = rx8130_time_is_valid;
    dev->base.deinit           = rx8130_deinit;
    dev->base.timer_start      = rx8130_timer_start;
    dev->base.timer_stop       = rx8130_timer_stop;
    dev->base.timer_is_expired = rx8130_timer_is_expired;
    dev->base.timer_clear      = rx8130_timer_clear;
    dev->base.set_int_cb       = config->int_io != GPIO_NUM_NC ? rx8130_set_int_cb : NULL;

    dev->lock = xSemaphoreCreateMutex();
    if (!dev->lock) { free(dev); return ESP_ERR_NO_MEM; }

    uint8_t addr = config->i2c_address ? config->i2c_address : RX8130_I2C_ADDR;
    esp_err_t err = i2c_master_probe(config->i2c_bus, addr, I2C_TIMEOUT_MS);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "no RX8130 at 0x%02x: %s", addr, esp_err_to_name(err));
        rx8130_deinit(&dev->base);
        return err;
    }
    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = addr,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : RX8130_I2C_DEFAULT_HZ,
    };
    err = i2c_master_bus_add_device(config->i2c_bus, &dev_cfg, &dev->i2c_dev);
    if (err != ESP_OK) { rx8130_deinit(&dev->base); return err; }

    err = chip_init(dev);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "chip init failed: %s", esp_err_to_name(err));
        rx8130_deinit(&dev->base);
        return err;
    }

    if (config->int_io != GPIO_NUM_NC) {
        err = int_setup(dev);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "INT setup failed: %s", esp_err_to_name(err));
            rx8130_deinit(&dev->base);
            return err;
        }
    }

    *out_rtc = &dev->base;
    return ESP_OK;
}
