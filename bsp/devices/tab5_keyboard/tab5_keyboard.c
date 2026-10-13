/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "tab5_keyboard.h"
#include "esp_log.h"
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

static const char *TAG = "tab5_keyboard";

#define REG_INT_STAT     0x01
#define REG_EVENT_NUM    0x02
#define REG_BRIGHTNESS   0x03
#define REG_MODE         0x10
#define REG_RGB_MODE     0x11
#define REG_KEY_EVENT    0x20
#define REG_HID_EVENT    0x30
#define REG_CHAR_LENGTH  0x40
#define REG_CHAR_EVENT   0x50
#define REG_RGB1         0x60
#define REG_RGB2         0x64

#define FW_MODE_NORMAL   0
#define FW_MODE_HID      1
#define FW_MODE_CHAR     2

#define RGB_MODE_CUSTOM  1
#define EVENT_EMPTY      0xFF
#define CHAR_EVENT_MAX   10
#define KEY_COLUMNS      14
#define LED_COUNT        2
#define PROBE_TIMEOUT_MS 50

typedef struct {
    bsp_keyboard_t          base;
    bsp_led_t               led;
    i2c_master_bus_handle_t bus;
    i2c_master_dev_handle_t device;
    uint8_t                 address;
    bsp_keyboard_mode_t     mode;
    bool                    present;
    bool                    rgb_custom;
    uint8_t                 rgb[LED_COUNT][3];
} tab5_keyboard_t;

static tab5_keyboard_t *from_led(bsp_led_t *led) {
    return (tab5_keyboard_t *)((char *)led - offsetof(tab5_keyboard_t, led));
}

static esp_err_t write_reg(tab5_keyboard_t *s, uint8_t reg, const uint8_t *data, size_t len) {
    uint8_t buf[1 + 3];
    if (len > sizeof(buf) - 1) return ESP_ERR_INVALID_SIZE;
    buf[0] = reg;
    memcpy(buf + 1, data, len);
    return i2c_master_transmit(s->device, buf, 1 + len, -1);
}

static esp_err_t write_reg8(tab5_keyboard_t *s, uint8_t reg, uint8_t value) {
    return write_reg(s, reg, &value, 1);
}

static esp_err_t read_reg(tab5_keyboard_t *s, uint8_t reg, uint8_t *data, size_t len) {
    return i2c_master_transmit_receive(s->device, &reg, 1, data, len, -1);
}

static void set_present(tab5_keyboard_t *s, bool present) {
    s->present   = present;
    s->led.count = present ? LED_COUNT : 0;
}

/* bsp_led levels are absolute; the firmware's global brightness (default 20%)
 * would scale them down. */
static esp_err_t enter_rgb_custom(tab5_keyboard_t *s) {
    esp_err_t err = write_reg8(s, REG_RGB_MODE, RGB_MODE_CUSTOM);
    if (err == ESP_OK) err = write_reg8(s, REG_BRIGHTNESS, 100);
    return err;
}

static esp_err_t write_rgb(tab5_keyboard_t *s, uint8_t index) {
    const uint8_t *c = s->rgb[index];
    const uint8_t bgr[3] = { c[2], c[1], c[0] };
    return write_reg(s, index == 0 ? REG_RGB1 : REG_RGB2, bgr, sizeof(bgr));
}

static esp_err_t restore_rgb(tab5_keyboard_t *s) {
    esp_err_t err = enter_rgb_custom(s);
    for (uint8_t i = 0; i < LED_COUNT && err == ESP_OK; i++) err = write_rgb(s, i);
    return err;
}

static esp_err_t kb_probe(bsp_keyboard_t *self) {
    tab5_keyboard_t *s = (tab5_keyboard_t *)self;
    esp_err_t err = i2c_master_probe(s->bus, s->address, PROBE_TIMEOUT_MS);
    if (err != ESP_OK) {
        set_present(s, false);
        return err;
    }
    if (!s->present && s->rgb_custom) {
        esp_err_t rgb_err = restore_rgb(s);
        if (rgb_err != ESP_OK) ESP_LOGW(TAG, "restore rgb: %s", esp_err_to_name(rgb_err));
    }
    set_present(s, true);
    return ESP_OK;
}

static esp_err_t kb_set_mode(bsp_keyboard_t *self, bsp_keyboard_mode_t mode) {
    tab5_keyboard_t *s = (tab5_keyboard_t *)self;
    uint8_t fw_mode;
    switch (mode) {
        case BSP_KEYBOARD_MODE_HID:  fw_mode = FW_MODE_HID;    break;
        case BSP_KEYBOARD_MODE_RAW:  fw_mode = FW_MODE_NORMAL; break;
        case BSP_KEYBOARD_MODE_CHAR: fw_mode = FW_MODE_CHAR;   break;
        default: return ESP_ERR_NOT_SUPPORTED;
    }
    esp_err_t err = write_reg8(s, REG_MODE, fw_mode);
    /* Writing the current mode is ignored by the firmware and keeps its queue. */
    if (err == ESP_OK) err = write_reg8(s, REG_EVENT_NUM, 0);
    if (err != ESP_OK) {
        set_present(s, false);
        return err;
    }
    s->mode = mode;
    return ESP_OK;
}

/* Returns false when the queue is empty. */
static bool read_event(tab5_keyboard_t *s, bsp_keyboard_event_t *out, esp_err_t *err) {
    uint8_t buf[CHAR_EVENT_MAX];
    switch (s->mode) {
        case BSP_KEYBOARD_MODE_RAW:
            if ((*err = read_reg(s, REG_KEY_EVENT, buf, 1)) != ESP_OK) return false;
            if (buf[0] == EVENT_EMPTY) return false;
            out->type        = BSP_KEYBOARD_EVENT_RAW;
            out->raw.index   = ((buf[0] >> 4) & 0x07) * KEY_COLUMNS + (buf[0] & 0x0F);
            out->raw.pressed = (buf[0] & 0x80) != 0;
            return true;

        case BSP_KEYBOARD_MODE_HID:
            if ((*err = read_reg(s, REG_HID_EVENT, buf, 2)) != ESP_OK) return false;
            if (buf[0] == EVENT_EMPTY && buf[1] == EVENT_EMPTY) return false;
            out->type          = BSP_KEYBOARD_EVENT_HID;
            out->hid.modifiers = buf[0];
            out->hid.keycode   = buf[1];
            return true;

        case BSP_KEYBOARD_MODE_CHAR: {
            /* The length already counts the modifier byte; the protocol PDF's
             * "+1" is wrong. */
            uint8_t len = 0;
            if ((*err = read_reg(s, REG_CHAR_LENGTH, &len, 1)) != ESP_OK) return false;
            if (len == 0) return false;
            if (len > CHAR_EVENT_MAX) {
                ESP_LOGE(TAG, "char event length %u", len);
                *err = ESP_ERR_INVALID_RESPONSE;
                return false;
            }
            if ((*err = read_reg(s, REG_CHAR_EVENT, buf, len)) != ESP_OK) return false;
            out->type          = BSP_KEYBOARD_EVENT_CHAR;
            out->chr.modifiers = buf[0];
            out->chr.len       = len - 1;
            memcpy(out->chr.str, buf + 1, len - 1);
            out->chr.str[len - 1] = '\0';
            return true;
        }
        default:
            *err = ESP_ERR_INVALID_STATE;
            return false;
    }
}

static esp_err_t kb_poll(bsp_keyboard_t *self, bsp_keyboard_event_t *out, uint8_t max, uint8_t *count) {
    tab5_keyboard_t *s = (tab5_keyboard_t *)self;
    esp_err_t err = ESP_OK;
    uint8_t n = 0;
    while (n < max && read_event(s, &out[n], &err)) n++;
    /* INT stays asserted until INT_STAT is cleared and is re-asserted only
     * while the queue is non-empty, so clear it after draining, never before. */
    if (err == ESP_OK && n < max) err = write_reg8(s, REG_INT_STAT, 0);
    if (err != ESP_OK) set_present(s, false);
    *count = n;
    return err;
}

static esp_err_t kb_deinit(bsp_keyboard_t *self) {
    tab5_keyboard_t *s = (tab5_keyboard_t *)self;
    i2c_master_bus_rm_device(s->device);
    free(s);
    return ESP_OK;
}

static esp_err_t led_set_rgb(bsp_led_t *self, uint8_t index, uint8_t r, uint8_t g, uint8_t b) {
    tab5_keyboard_t *s = from_led(self);
    if (!s->present) return ESP_ERR_INVALID_STATE;
    if (!s->rgb_custom) {
        esp_err_t err = enter_rgb_custom(s);
        if (err != ESP_OK) return err;
        s->rgb_custom = true;
    }
    s->rgb[index][0] = r;
    s->rgb[index][1] = g;
    s->rgb[index][2] = b;
    return write_rgb(s, index);
}

static esp_err_t led_set_brightness(bsp_led_t *self, uint8_t index, uint8_t level) {
    return led_set_rgb(self, index, level, level, level);
}

static esp_err_t led_clear(bsp_led_t *self) {
    esp_err_t err = ESP_OK;
    for (uint8_t i = 0; i < LED_COUNT && err == ESP_OK; i++) err = led_set_rgb(self, i, 0, 0, 0);
    return err;
}

static esp_err_t led_deinit(bsp_led_t *self) {
    (void)self;
    return ESP_OK;
}

esp_err_t tab5_keyboard_create(const tab5_keyboard_config_t *config,
                               bsp_keyboard_t **out_keyboard, bsp_led_t **out_led) {
    if (!config || !config->i2c_bus || !out_keyboard) return ESP_ERR_INVALID_ARG;

    tab5_keyboard_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;
    s->bus     = config->i2c_bus;
    s->address = config->i2c_address ? config->i2c_address : TAB5_KEYBOARD_I2C_ADDR;

    const i2c_device_config_t dev_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = s->address,
        .scl_speed_hz    = config->clock_hz ? config->clock_hz : TAB5_KEYBOARD_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(s->bus, &dev_config, &s->device);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "add device: %s", esp_err_to_name(err));
        free(s);
        return err;
    }

    if (config->int_io >= 0) {
        /* The pull-up keeps INT idle while the keyboard is detached. */
        const gpio_config_t io = {
            .pin_bit_mask = 1ULL << config->int_io,
            .mode         = GPIO_MODE_INPUT,
            .pull_up_en   = GPIO_PULLUP_ENABLE,
        };
        err = gpio_config(&io);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "INT gpio: %s", esp_err_to_name(err));
            i2c_master_bus_rm_device(s->device);
            free(s);
            return err;
        }
    }

    s->base.caps     = BSP_KEYBOARD_CAP_HID | BSP_KEYBOARD_CAP_RAW | BSP_KEYBOARD_CAP_CHAR;
    s->base.int_io   = config->int_io;
    s->base.probe    = kb_probe;
    s->base.set_mode = kb_set_mode;
    s->base.poll     = kb_poll;
    s->base.deinit   = kb_deinit;

    s->led.set_rgb        = led_set_rgb;
    s->led.set_brightness = led_set_brightness;
    s->led.clear          = led_clear;
    s->led.deinit         = led_deinit;

    *out_keyboard = &s->base;
    if (out_led) *out_led = &s->led;
    return ESP_OK;
}
