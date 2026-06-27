/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic HotKnot dispatch through the active provider's vtable (mirror of
 * src/bsp_touch.c). No provider -> ESP_ERR_NOT_SUPPORTED.
 */

#include "bsp.h"
#include "bsp_hotknot.h"

static bsp_hotknot_t *s_hotknot;

void bsp_hotknot_set_active(bsp_hotknot_t *hk) {
    s_hotknot = hk;
}

esp_err_t bsp_hotknot_begin(bsp_hotknot_role_t role, bsp_hotknot_event_cb_t cb, void *arg) {
    if (!s_hotknot || !s_hotknot->begin) return ESP_ERR_NOT_SUPPORTED;
    return s_hotknot->begin(s_hotknot, role, cb, arg);
}

esp_err_t bsp_hotknot_send(const void *data, size_t len, uint32_t timeout_ms) {
    if (!s_hotknot || !s_hotknot->send) return ESP_ERR_NOT_SUPPORTED;
    if (!data || len == 0) return ESP_ERR_INVALID_ARG;
    return s_hotknot->send(s_hotknot, data, len, timeout_ms);
}

esp_err_t bsp_hotknot_end(void) {
    if (!s_hotknot || !s_hotknot->end) return ESP_ERR_NOT_SUPPORTED;
    return s_hotknot->end(s_hotknot);
}
