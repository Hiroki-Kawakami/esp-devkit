/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal HotKnot vtable (same shape as bsp_display/touch/rtc): a provider embeds
 * bsp_hotknot_t first and registers via bsp_hotknot_set_active; src/bsp_hotknot.c
 * dispatches the public API through it.
 */

#pragma once
#include "bsp_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct bsp_hotknot bsp_hotknot_t;

struct bsp_hotknot {
    esp_err_t (*begin)(bsp_hotknot_t *self, bsp_hotknot_role_t role,
                       bsp_hotknot_event_cb_t cb, void *arg);
    esp_err_t (*send)(bsp_hotknot_t *self, const void *data, size_t len, uint32_t timeout_ms);
    esp_err_t (*end)(bsp_hotknot_t *self);
};

/* Register the active HotKnot provider (the board's bsp_init, after touch). */
void bsp_hotknot_set_active(bsp_hotknot_t *hk);

#ifdef __cplusplus
}
#endif
