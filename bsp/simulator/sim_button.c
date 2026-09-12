/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sim_button.h"
#include <stdlib.h>

static esp_err_t sim_button_sample(bsp_button_raw_t *self, bool *pressed, uint8_t max) {
    if (max > self->count) max = self->count;
    for (uint8_t i = 0; i < max; i++) pressed[i] = false;
    return ESP_OK;
}

static esp_err_t sim_button_deinit(bsp_button_raw_t *self) {
    free(self);
    return ESP_OK;
}

esp_err_t sim_button_create(uint8_t count, bsp_button_raw_t **out_button) {
    if (!out_button || count == 0) return ESP_ERR_INVALID_ARG;
    bsp_button_raw_t *b = calloc(1, sizeof(*b));
    if (!b) return ESP_ERR_NO_MEM;
    b->count   = count;
    b->sample  = sim_button_sample;
    b->deinit  = sim_button_deinit;
    b->has_int = true;   /* injection notifies the source; nothing to poll otherwise */
    *out_button = b;
    return ESP_OK;
}
