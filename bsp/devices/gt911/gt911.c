/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT911 grayscale-panel touch driver -- see gt911.h. Polling reads over I2C with
 * an optional INT fast path and a background reader task; the task also hosts the
 * HotKnot session step (gt911_internal.h) so touch and HotKnot share one chip owner.
 */

#include "gt911.h"
#include "gt911_internal.h"
#include "bsp_touch.h"
#include <stdlib.h>
#include <string.h>
#include "esp_check.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

static const char *TAG = "gt911";

/* ---- Registers ---------------------------------------------------------- */
#define REG_COMMAND       0x8040u  /* write 0x05 to sleep                    */
#define REG_PRODUCT_ID    0x8140u  /* 4 ASCII bytes                          */
#define REG_STATUS        0x814Eu  /* bit7 = data ready, bits[3:0] = count   */
#define REG_TOUCH_DATA    0x814Fu  /* 8 bytes per point, up to 5 points      */

#define STATUS_READY      0x80u
#define STATUS_COUNT_MASK 0x0Fu
#define POINT_SIZE        8u

/* ---- Timing ------------------------------------------------------------- */
#define RESET_LOW_US      200u    /* datasheet: >100us                       */
#define ADDR_HOLD_MS      5u      /* INT level held after RESET rising edge  */
#define BOOT_MS           50u     /* let firmware come up before I2C         */
#define INT_SYNC_LOW_MS   100u    /* INT held low to reboot touch firmware   */
#define I2C_TIMEOUT_MS    50
#define POLL_INTERVAL_MS  10      /* reader-task poll fallback when INT idles  */
#define TASK_STACK_DEFAULT 3072

typedef struct {
    uint8_t  track_id;
    uint16_t x;
    uint16_t y;
    uint16_t size;
} gt911_point_t;

struct gt911_dev {
    bsp_touch_t             base;     /* vtable; must be first                */
    gt911_config_t          cfg;
    i2c_master_dev_handle_t i2c_dev;
    uint8_t                 i2c_address;
    bool                    int_is_input;
    SemaphoreHandle_t       lock;
    SemaphoreHandle_t       irq;      /* given by the INT ISR; NULL if no INT */

    /* Optional background reader task (pushes samples to bsp_touch_emit_event). */
    TaskHandle_t            task;
    SemaphoreHandle_t       task_done; /* task signals exit on deinit          */
    volatile bool           task_stop;
    bool                    has_task;  /* task owns acquisition when set        */

    /* HotKnot session step: when set, the reader task calls it instead of doing
     * normal touch handling (see gt911_internal_set_session_step). */
    volatile gt911_session_step_fn session_step;
    void                          *session_ctx;

    /* The chip's last coord poll. `fresh` is set when a poll read a new frame
     * (count may be 0 -- an all-released event). `pair_event` latches a HotKnot
     * pair (track id 32), consumed by gt911_internal_service_touch. */
    gt911_point_t           points[GT911_MAX_TOUCH_POINTS];
    uint8_t                 count;
    bool                    fresh;
    volatile bool           pair_event;
};

/* One GT911 per board; the HotKnot provider binds to it via gt911_active_handle(). */
static struct gt911_dev *s_active;

/* ========================================================================
 * I2C primitives — GT911 uses 16-bit big-endian register addresses.
 * Locked variants assume the caller already holds dev->lock.
 * ===================================================================== */

static esp_err_t reg_read_locked(struct gt911_dev *dev, uint16_t reg, uint8_t *buf, size_t len) {
    uint8_t addr[2] = { (uint8_t)(reg >> 8), (uint8_t)reg };
    return i2c_master_transmit_receive(dev->i2c_dev, addr, 2, buf, len, I2C_TIMEOUT_MS);
}

static esp_err_t reg_write_locked(struct gt911_dev *dev, uint16_t reg, const uint8_t *buf, size_t len) {
    uint8_t scratch[16];
    if (len + 2 > sizeof(scratch)) return ESP_ERR_INVALID_SIZE;
    scratch[0] = (uint8_t)(reg >> 8);
    scratch[1] = (uint8_t)reg;
    if (len) memcpy(&scratch[2], buf, len);
    return i2c_master_transmit(dev->i2c_dev, scratch, len + 2, I2C_TIMEOUT_MS);
}

static inline esp_err_t reg_write_byte_locked(struct gt911_dev *dev, uint16_t reg, uint8_t value) {
    return reg_write_locked(dev, reg, &value, 1);
}

static inline void dev_lock(struct gt911_dev *dev)   { if (dev->lock) xSemaphoreTake(dev->lock, portMAX_DELAY); }
static inline void dev_unlock(struct gt911_dev *dev) { if (dev->lock) xSemaphoreGive(dev->lock); }

/* ========================================================================
 * GPIO / reset / INT
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
    /* Input-only pads (ESP32 GPIO34..39) have no internal pull resistor;
     * asking for one logs a "GPIO number error". The board's external pull-up
     * keeps the line idle high, so silently drop the request there. */
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
    struct gt911_dev *dev = arg;
    BaseType_t hp = pdFALSE;
    if (dev->irq) xSemaphoreGiveFromISR(dev->irq, &hp);
    if (hp) portYIELD_FROM_ISR();
}

/* INT idles high; the chip drives it low when a fresh sample is queued. After
 * the line is an input, attach a falling-edge ISR so bsp_touch_wait_interrupt
 * can block until data is ready. Best-effort: failure leaves polling working. */
static void int_enable_irq(struct gt911_dev *dev) {
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

/* Re-arm the data-ready interrupt after a reset / int_sync flipped the pin
 * through output mode (which clears the edge config). The ISR handler stays
 * registered from int_enable_irq; only the edge type + enable need restoring. */
static void int_rearm_irq(struct gt911_dev *dev) {
    if (dev->cfg.int_io == GPIO_NUM_NC || !dev->int_is_input || !dev->irq) return;
    gpio_set_intr_type(dev->cfg.int_io, GPIO_INTR_NEGEDGE);
    gpio_intr_enable(dev->cfg.int_io);
}

/* The chip latches its I2C address from INT level at the RESET rising edge:
 *   INT high -> 0x14, INT low -> 0x5D.
 * We can only force the address when both pins are wired AND INT is output-
 * capable (input-only pads around GPIO34-39 are common for INT). When RESET is
 * not wired (M5Paper / M5PaperS3), this only re-asserts INT as an input — the
 * chip is not actually reset, so a HotKnot subsystem can only be cleared by a
 * power cycle. */
static esp_err_t hw_reset(struct gt911_dev *dev, uint8_t target_addr) {
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
        int level = (target_addr == GT911_I2C_ADDR_SECONDARY) ? 0 : 1;
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

static esp_err_t attach_device(struct gt911_dev *dev, uint8_t addr) {
    if (dev->i2c_dev) {
        i2c_master_bus_rm_device(dev->i2c_dev);
        dev->i2c_dev = NULL;
    }
    i2c_device_config_t cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = addr,
        .scl_speed_hz    = dev->cfg.clock_hz > 0 ? dev->cfg.clock_hz : GT911_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(dev->cfg.i2c_bus, &cfg, &dev->i2c_dev);
    if (err == ESP_OK) dev->i2c_address = addr;
    return err;
}

static esp_err_t probe_and_attach(struct gt911_dev *dev, uint8_t hint) {
    uint8_t candidates[2];
    size_t n = 0;
    if (hint == GT911_I2C_ADDR_PRIMARY || hint == GT911_I2C_ADDR_SECONDARY) {
        candidates[n++] = hint;
        candidates[n++] = (hint == GT911_I2C_ADDR_PRIMARY)
                              ? GT911_I2C_ADDR_SECONDARY : GT911_I2C_ADDR_PRIMARY;
    } else {
        candidates[n++] = GT911_I2C_ADDR_PRIMARY;
        candidates[n++] = GT911_I2C_ADDR_SECONDARY;
    }
    for (size_t i = 0; i < n; i++) {
        if (i2c_master_probe(dev->cfg.i2c_bus, candidates[i], I2C_TIMEOUT_MS) == ESP_OK) {
            return attach_device(dev, candidates[i]);
        }
    }
    return ESP_ERR_NOT_FOUND;
}

static void log_info_locked(struct gt911_dev *dev) {
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
 * Coord poll
 *
 * Reads 0x814E once. On a fresh sample, reads all touch points, classifies each
 * as a normal touch (cached in points[]) or a HotKnot pair event (latched in
 * pair_event), sets `fresh`, and clears the chip's data-ready latch. Idempotent.
 * Caller must hold dev->lock.
 * ===================================================================== */
static esp_err_t coord_poll_locked(struct gt911_dev *dev) {
    dev->fresh = false;

    uint8_t status = 0;
    esp_err_t err = reg_read_locked(dev, REG_STATUS, &status, 1);
    if (err != ESP_OK) return err;
    if (!(status & STATUS_READY)) return ESP_OK;

    uint8_t n = status & STATUS_COUNT_MASK;
    if (n > GT911_MAX_TOUCH_POINTS) n = GT911_MAX_TOUCH_POINTS;

    dev->count = 0;
    dev->fresh = true;
    if (n > 0) {
        uint8_t raw[GT911_MAX_TOUCH_POINTS * POINT_SIZE];
        err = reg_read_locked(dev, REG_TOUCH_DATA, raw, (size_t)n * POINT_SIZE);
        if (err != ESP_OK) {
            /* Always clear the latch so a transient glitch doesn't wedge us
             * reporting stale data forever. */
            (void)reg_write_byte_locked(dev, REG_STATUS, 0);
            return err;
        }
        for (uint8_t i = 0; i < n; i++) {
            const uint8_t *p = &raw[i * POINT_SIZE];
            if (p[0] == GT911_HK_PAIR_TRACK_ID) {
                /* Approach mode overloads track id 32 as a pair event; the x
                 * bytes carry proximity state (bit7 of both set = valid), not a
                 * coord. Drop it from the touch list and latch the pair. */
                if ((p[1] & 0x80) && (p[2] & 0x80)) dev->pair_event = true;
                continue;
            }
            gt911_point_t *out = &dev->points[dev->count++];
            out->track_id = p[0];
            out->x        = (uint16_t)p[1] | ((uint16_t)p[2] << 8);
            out->y        = (uint16_t)p[3] | ((uint16_t)p[4] << 8);
            out->size     = (uint16_t)p[5] | ((uint16_t)p[6] << 8);
        }
    }

    return reg_write_byte_locked(dev, REG_STATUS, 0);
}

/* Map raw chip coordinates into display space per the orientation config. */
static void transform(const struct gt911_dev *dev, const gt911_point_t *in, bsp_touch_point_t *out) {
    int x = in->x, y = in->y;
    if (dev->cfg.swap_xy) { int t = x; x = y; y = t; }
    if (dev->cfg.mirror_x && dev->cfg.width)  x = dev->cfg.width  - 1 - x;
    if (dev->cfg.mirror_y && dev->cfg.height) y = dev->cfg.height - 1 - y;
    out->x  = x;
    out->y  = y;
    out->id = in->track_id;
}

/* ========================================================================
 * bsp_touch_t vtable
 * ===================================================================== */

/* Reader task: while a HotKnot session step is installed it owns each iteration;
 * otherwise push each fresh touch sample (display space) to bsp_touch_emit_event.
 * INT-woken with a poll-interval fallback (also the heartbeat for the HotKnot
 * receive poll, which the chip never signals via INT). */
static void touch_task(void *arg) {
    struct gt911_dev *dev = arg;
    uint32_t ms = dev->cfg.acquire.poll_interval_ms;
    const TickType_t timeout = pdMS_TO_TICKS(ms ? ms : POLL_INTERVAL_MS);
    while (!dev->task_stop) {
        if (dev->irq) xSemaphoreTake(dev->irq, timeout);
        else          vTaskDelay(timeout);
        if (dev->task_stop) break;

        gt911_session_step_fn step = dev->session_step;
        if (step) step(dev, dev->session_ctx);
        else      gt911_internal_service_touch(dev, NULL);
    }
    xSemaphoreGive(dev->task_done);
    vTaskDelete(NULL);
}

static int gt911_read(bsp_touch_t *self, bsp_touch_point_t *points, uint8_t max_points) {
    struct gt911_dev *dev = (struct gt911_dev *)self;
    if (dev->has_task) return 0;   /* task owns the chip; inline poll would race it */

    int out = 0;
    dev_lock(dev);
    if (coord_poll_locked(dev) == ESP_OK && dev->fresh) {
        uint8_t n = dev->count;
        if (n > max_points) n = max_points;
        for (uint8_t i = 0; i < n; i++) transform(dev, &dev->points[i], &points[i]);
        out = n;
    }
    dev_unlock(dev);
    return out;
}

static void gt911_wait_interrupt(bsp_touch_t *self) {
    struct gt911_dev *dev = (struct gt911_dev *)self;
    /* Block until the INT ISR signals a fresh sample. No timeout — the binary
     * semaphore latches an edge that arrived before the wait, so a pending
     * sample returns immediately and we never miss one. */
    if (!dev->irq) { vTaskDelay(pdMS_TO_TICKS(10)); return; }
    xSemaphoreTake(dev->irq, portMAX_DELAY);
}

static esp_err_t gt911_deinit(bsp_touch_t *self) {
    struct gt911_dev *dev = (struct gt911_dev *)self;
    if (!dev) return ESP_OK;
    if (dev->has_task) {
        dev->task_stop = true;
        if (dev->irq) xSemaphoreGive(dev->irq);   /* wake it out of the INT wait */
        xSemaphoreTake(dev->task_done, portMAX_DELAY);
        vSemaphoreDelete(dev->task_done);
    }
    if (dev->cfg.int_io != GPIO_NUM_NC && dev->int_is_input) {
        gpio_intr_disable(dev->cfg.int_io);
        gpio_isr_handler_remove(dev->cfg.int_io);
    }
    if (s_active == dev) s_active = NULL;
    if (dev->i2c_dev) i2c_master_bus_rm_device(dev->i2c_dev);
    if (dev->irq)     vSemaphoreDelete(dev->irq);
    if (dev->lock)    vSemaphoreDelete(dev->lock);
    free(dev);
    return ESP_OK;
}

/* ========================================================================
 * Internal seam (gt911_internal.h) — used by the HotKnot sibling files.
 * ===================================================================== */

gt911_handle_t gt911_active_handle(void) {
    /* HotKnot needs the reader task to drive its state machine; without one the
     * chip would have no owner, so report no usable device. */
    return (s_active && s_active->has_task) ? s_active : NULL;
}

bool gt911_internal_has_task(gt911_handle_t h) { return h && h->has_task; }

void gt911_internal_lock(gt911_handle_t h)   { if (h) dev_lock(h); }
void gt911_internal_unlock(gt911_handle_t h) { if (h) dev_unlock(h); }

esp_err_t gt911_read_reg(gt911_handle_t h, uint16_t reg, uint8_t *buf, size_t len) {
    if (!h || (len && !buf)) return ESP_ERR_INVALID_ARG;
    dev_lock(h);
    esp_err_t err = reg_read_locked(h, reg, buf, len);
    dev_unlock(h);
    return err;
}

esp_err_t gt911_internal_read_reg_locked(gt911_handle_t h, uint16_t reg, uint8_t *buf, size_t len) {
    if (!h || (len && !buf)) return ESP_ERR_INVALID_ARG;
    return reg_read_locked(h, reg, buf, len);
}

/* reg_write_locked()'s scratch is small; spill longer writes (the 186-byte
 * HotKnot config patch, 256-byte FW chunks) to the heap. */
static esp_err_t write_reg_spill(gt911_handle_t h, uint16_t reg, const uint8_t *buf, size_t len) {
    if (len + 2 <= 16) return reg_write_locked(h, reg, buf, len);
    uint8_t *scratch = malloc(len + 2);
    if (!scratch) return ESP_ERR_NO_MEM;
    scratch[0] = (uint8_t)(reg >> 8);
    scratch[1] = (uint8_t)reg;
    if (len) memcpy(&scratch[2], buf, len);
    esp_err_t err = i2c_master_transmit(h->i2c_dev, scratch, len + 2, I2C_TIMEOUT_MS);
    free(scratch);
    return err;
}

esp_err_t gt911_write_reg(gt911_handle_t h, uint16_t reg, const uint8_t *buf, size_t len) {
    if (!h || (len && !buf)) return ESP_ERR_INVALID_ARG;
    dev_lock(h);
    esp_err_t err = write_reg_spill(h, reg, buf, len);
    dev_unlock(h);
    return err;
}

esp_err_t gt911_internal_write_reg_locked(gt911_handle_t h, uint16_t reg, const uint8_t *buf, size_t len) {
    if (!h || (len && !buf)) return ESP_ERR_INVALID_ARG;
    return write_reg_spill(h, reg, buf, len);
}

esp_err_t gt911_internal_write_byte_locked(gt911_handle_t h, uint16_t reg, uint8_t value) {
    if (!h) return ESP_ERR_INVALID_ARG;
    return reg_write_byte_locked(h, reg, value);
}

const gt911_hotknot_tuning_t *gt911_internal_tuning(gt911_handle_t h) {
    return h ? &h->cfg.hotknot : NULL;
}

int gt911_internal_int_level(gt911_handle_t h) {
    if (!h || h->cfg.int_io == GPIO_NUM_NC || !h->int_is_input) return 1;
    return gpio_get_level(h->cfg.int_io);
}

void gt911_internal_service_touch(gt911_handle_t dev, bool *out_paired) {
    bsp_touch_point_t pts[GT911_MAX_TOUCH_POINTS];
    int n = -1;
    bool paired = false;
    dev_lock(dev);
    if (coord_poll_locked(dev) == ESP_OK) {
        if (dev->pair_event) { paired = true; dev->pair_event = false; }
        if (dev->fresh) {
            n = dev->count;
            for (int i = 0; i < n; i++) transform(dev, &dev->points[i], &pts[i]);
            dev->fresh = false;
        }
    }
    dev_unlock(dev);
    if (n >= 0)     bsp_touch_emit_event(pts, n);  /* outside the lock */
    if (out_paired) *out_paired = paired;
}

void gt911_internal_set_session_step(gt911_handle_t h, gt911_session_step_fn fn, void *ctx) {
    if (!h) return;
    h->session_ctx  = ctx;
    h->session_step = fn;
}

esp_err_t gt911_reset(gt911_handle_t h) {
    ESP_RETURN_ON_FALSE(h, ESP_ERR_INVALID_ARG, TAG, "null arg");
    dev_lock(h);
    esp_err_t err = hw_reset(h, h->cfg.i2c_address);
    if (err == ESP_OK) {
        err = probe_and_attach(h, h->cfg.i2c_address);
        if (err == ESP_OK) {
            h->count = 0; h->fresh = false; h->pair_event = false;
            log_info_locked(h);
        } else {
            ESP_LOGE(TAG, "no GT911 on bus after reset");
        }
    }
    dev_unlock(h);
    if (err == ESP_OK) int_rearm_irq(h);
    return err;
}

esp_err_t gt911_int_sync(gt911_handle_t h) {
    ESP_RETURN_ON_FALSE(h, ESP_ERR_INVALID_ARG, TAG, "null arg");
    if (h->cfg.int_io == GPIO_NUM_NC || !GPIO_IS_VALID_OUTPUT_GPIO(h->cfg.int_io)) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    esp_err_t err = gpio_as_output(h->cfg.int_io, 0);
    if (err != ESP_OK) return err;
    h->int_is_input = false;
    vTaskDelay(pdMS_TO_TICKS(INT_SYNC_LOW_MS));
    err = gpio_as_input(h->cfg.int_io, true);
    if (err != ESP_OK) return err;
    h->int_is_input = true;
    vTaskDelay(pdMS_TO_TICKS(BOOT_MS));
    int_rearm_irq(h);
    return ESP_OK;
}

/* ========================================================================
 * Lifecycle
 * ===================================================================== */

esp_err_t gt911_touch_create(const gt911_config_t *config, bsp_touch_t **out_touch) {
    ESP_RETURN_ON_FALSE(config && out_touch, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(config->i2c_bus, ESP_ERR_INVALID_ARG, TAG, "i2c_bus required");

    struct gt911_dev *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;
    dev->base.read           = gt911_read;
    dev->base.wait_interrupt = gt911_wait_interrupt;
    dev->base.deinit         = gt911_deinit;

    dev->lock = xSemaphoreCreateMutex();
    if (!dev->lock) { free(dev); return ESP_ERR_NO_MEM; }
    if (config->int_io != GPIO_NUM_NC) dev->irq = xSemaphoreCreateBinary();

    esp_err_t err = hw_reset(dev, config->i2c_address);
    if (err == ESP_OK) err = probe_and_attach(dev, config->i2c_address);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "no GT911 on bus: %s", esp_err_to_name(err));
        gt911_deinit(&dev->base);
        return err;
    }

    dev_lock(dev);
    log_info_locked(dev);
    dev_unlock(dev);

    int_enable_irq(dev);

    if (config->acquire.task_priority > 0) {
        const gt911_acquire_config_t *a = &config->acquire;
        dev->task_done = xSemaphoreCreateBinary();
        BaseType_t core = a->task_affinity < 0 ? tskNO_AFFINITY : a->task_affinity;
        uint32_t stack = a->task_stack ? a->task_stack : TASK_STACK_DEFAULT;
        if (dev->task_done &&
            xTaskCreatePinnedToCore(touch_task, "gt911", stack, dev,
                                    a->task_priority, &dev->task, core) == pdPASS) {
            dev->has_task = true;
        } else {
            ESP_LOGW(TAG, "reader task create failed; using on-demand polling");
            if (dev->task_done) { vSemaphoreDelete(dev->task_done); dev->task_done = NULL; }
        }
    }

    s_active = dev;
    *out_touch = &dev->base;
    return ESP_OK;
}
