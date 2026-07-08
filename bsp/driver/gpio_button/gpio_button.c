/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "gpio_button.h"
#include <stdlib.h>
#include "esp_check.h"
#include "bsp_button.h"
#include "bsp_dispatch.h"

static const char *TAG = "gpio_button";

#define GPIO_BUTTON_MAX_COUNT   8

struct gpio_button_dev {
    bsp_button_t      base;
    gpio_button_pin_t pins[GPIO_BUTTON_MAX_COUNT];
};

static void IRAM_ATTR gpio_button_isr(void *arg) {
    (void)arg;
    BaseType_t hp = pdFALSE;
    bsp_button_notify_from_isr(&hp);
    if (hp) portYIELD_FROM_ISR();
}

/* Best-effort: on failure has_int stays false and the common layer keeps polling. */
static void attach_isr(struct gpio_button_dev *dev) {
    if (bsp_dispatch_install_gpio_isr() != ESP_OK) return;
    for (uint8_t i = 0; i < dev->base.count; i++) {
        gpio_num_t pin = dev->pins[i].gpio;
        gpio_set_intr_type(pin, GPIO_INTR_ANYEDGE);
        if (gpio_isr_handler_add(pin, gpio_button_isr, dev) != ESP_OK) return;
        gpio_intr_enable(pin);
    }
    dev->base.has_int = true;
}

static esp_err_t gpio_button_sample(bsp_button_t *self, bool *pressed, uint8_t max) {
    struct gpio_button_dev *dev = (struct gpio_button_dev *)self;
    if (max > dev->base.count) max = dev->base.count;
    for (uint8_t i = 0; i < max; i++) {
        int level = gpio_get_level(dev->pins[i].gpio);
        pressed[i] = dev->pins[i].active_low ? (level == 0) : (level != 0);
    }
    return ESP_OK;
}

static esp_err_t gpio_button_deinit(bsp_button_t *self) {
    struct gpio_button_dev *dev = (struct gpio_button_dev *)self;
    if (dev->base.has_int) {
        for (uint8_t i = 0; i < dev->base.count; i++)
            gpio_isr_handler_remove(dev->pins[i].gpio);
    }
    free(dev);
    return ESP_OK;
}

esp_err_t gpio_button_create(const gpio_button_config_t *cfg, bsp_button_t **out_button) {
    ESP_RETURN_ON_FALSE(cfg && out_button && cfg->pins, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(cfg->count > 0 && cfg->count <= GPIO_BUTTON_MAX_COUNT,
                        ESP_ERR_INVALID_ARG, TAG, "bad count");

    struct gpio_button_dev *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->base.count  = cfg->count;
    dev->base.sample = gpio_button_sample;
    dev->base.deinit = gpio_button_deinit;

    uint64_t mask = 0;
    bool all_active_low = true, all_active_high = true;
    for (uint8_t i = 0; i < cfg->count; i++) {
        dev->pins[i] = cfg->pins[i];
        mask |= 1ULL << cfg->pins[i].gpio;
        if (cfg->pins[i].active_low) all_active_high = false;
        else                         all_active_low  = false;
    }

    /* gpio_config applies one pull to the whole mask, so a pull only makes
     * sense when every button releases to the same level (mixed -> external). */
    const gpio_config_t io = {
        .pin_bit_mask = mask,
        .mode         = GPIO_MODE_INPUT,
        .pull_up_en   = (cfg->enable_pull && all_active_low)  ? GPIO_PULLUP_ENABLE
                                                              : GPIO_PULLUP_DISABLE,
        .pull_down_en = (cfg->enable_pull && all_active_high) ? GPIO_PULLDOWN_ENABLE
                                                              : GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&io);
    if (err != ESP_OK) {
        free(dev);
        return err;
    }

    attach_isr(dev);

    *out_button = &dev->base;
    return ESP_OK;
}
