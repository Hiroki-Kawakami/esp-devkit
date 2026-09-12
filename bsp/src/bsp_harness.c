/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sdkconfig.h"
#if CONFIG_BSP_HARNESS

#include "bsp.h"
#include "bsp_harness.h"
#include "bsp_display.h"
#include "bsp_touch.h"

static bsp_harness_screenshot_fn s_screenshot_fn;
static void                     *s_screenshot_user;

void bsp_harness_touch_inject(const bsp_touch_point_t *points, int count) {
    bsp_touch_emit_event(points, count);
}

esp_err_t bsp_harness_display_read(bsp_rect_t area, void *pixels) {
    bsp_display_t *d = bsp_display_get_active();
    if (!d || !d->read_bitmap) return ESP_ERR_NOT_SUPPORTED;
    if (!pixels || area.size.width <= 0 || area.size.height <= 0 ||
        area.origin.x < 0 || area.origin.y < 0 ||
        area.origin.x + area.size.width > d->size.width ||
        area.origin.y + area.size.height > d->size.height) {
        return ESP_ERR_INVALID_ARG;
    }
    return d->read_bitmap(d, area, pixels);
}

void bsp_harness_set_screenshot_cb(bsp_harness_screenshot_fn fn, void *user) {
    s_screenshot_fn   = fn;
    s_screenshot_user = user;
}

bool bsp_harness_screenshot(const char *path) {
    return s_screenshot_fn ? s_screenshot_fn(path, s_screenshot_user) : false;
}

#endif /* CONFIG_BSP_HARNESS */
