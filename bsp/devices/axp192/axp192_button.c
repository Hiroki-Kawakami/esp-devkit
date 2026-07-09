/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AXP192 power-key button provider. The PMIC has no INT line to the host, so the
 * key is polled over I2C; the discrete short/long press events map to CLICK /
 * LONG_PRESS. A tick-driven bsp_button provider (bsp_button_add).
 */

#include "axp192.h"
#include <stdlib.h>

#define POLL_MS  80

typedef struct {
    bsp_button_t    base;
    axp192_handle_t axp;
} axp192_button_t;

static uint32_t button_tick(bsp_button_t *self) {
    axp192_button_t *b = (axp192_button_t *)self;
    bool sp = false, lp = false;
    if (axp192_poll_power_key(b->axp, &sp, &lp) == ESP_OK) {
        if (sp) bsp_button_emit(self->base_id, BSP_BUTTON_EVENT_CLICK);
        if (lp) bsp_button_emit(self->base_id, BSP_BUTTON_EVENT_LONG_PRESS);
    }
    return POLL_MS;
}

/* Mirror the registered long-press duration into the PMIC's hardware threshold. */
static void button_config(bsp_button_t *self, uint8_t local_id, const bsp_button_reg_t *reg) {
    (void)local_id;
    axp192_button_t *b = (axp192_button_t *)self;
    if (reg->long_press) axp192_set_pek_long_ms(b->axp, reg->long_ms);
}

static esp_err_t button_deinit(bsp_button_t *self) {
    free(self);
    return ESP_OK;
}

esp_err_t axp192_button_create(axp192_handle_t h, bsp_button_t **out_button) {
    if (!h || !out_button) return ESP_ERR_INVALID_ARG;
    axp192_button_t *b = calloc(1, sizeof(*b));
    if (!b) return ESP_ERR_NO_MEM;
    b->axp            = h;
    b->base.count     = 1;
    b->base.tick      = button_tick;
    b->base.on_config = button_config;
    b->base.deinit    = button_deinit;
    *out_button = &b->base;
    return ESP_OK;
}
