/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Test-harness hooks into the BSP (CONFIG_BSP_HARNESS). Everything here feeds
 * the same paths real hardware does -- an injected touch goes through the touch
 * layer's event/snapshot delivery, an injected button level through the shared
 * debounce/click state machine -- so the app cannot tell a scripted input from
 * a finger. Display readback returns what the panel currently holds, in its own
 * pixel format, without the BSP keeping a shadow copy. The libs/harness
 * component drives these over the console (device) or stdio (simulator).
 */

#pragma once
#include "bsp_types.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Whether a touch provider is registered (the sim boards without a panel and
 * the display-less devices have none). */
bool bsp_harness_touch_present(void);

/* Replace the synthetic contact set: `points` are display-space, `count` 0
 * releases everything. Delivered exactly like a chip poll (event callback +
 * bsp_touch_read snapshot). A real chip report later overrides it. */
void bsp_harness_touch_inject(const bsp_touch_point_t *points, int count);

/* Hold/release physical button `id` (bsp_button id space). Level providers
 * OR the injected state into their sample, so click/double/long-press derive
 * as usual; event-only providers get DOWN/UP directly. */
esp_err_t bsp_harness_button_inject(uint8_t id, bool pressed);

/* Copy the panel-coordinate rect `area` of the shown image into `pixels`
 * (tightly packed, bsp_display_get_pixel_format(), rotation 0). Needs
 * BSP_DISPLAY_CAP_READBACK; ESP_ERR_NOT_SUPPORTED otherwise. Reading the SPI
 * glass takes the bus, so callers must keep draws away meanwhile. */
esp_err_t bsp_harness_display_read(bsp_rect_t area, void *pixels);

/* Simulator only: the SDL window's `s` key asks the registered callback to save
 * a screenshot to `path` (relative to the working directory). */
typedef bool (*bsp_harness_screenshot_fn)(const char *path, void *user);
void bsp_harness_set_screenshot_cb(bsp_harness_screenshot_fn fn, void *user);
bool bsp_harness_screenshot(const char *path);

#ifdef __cplusplus
}
#endif
