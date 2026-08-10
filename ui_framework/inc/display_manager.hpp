/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * BSP-backed LVGL display construction. DisplayManager derives the physical
 * output path from BSP capabilities and builds the operation sequence for the
 * requested viewport and presentation timing.
 */

#pragma once

#include <stdint.h>

#include "bsp_types.h"
#include "esp_err.h"
#include "lvgl.h"

enum class DisplayPresentMode : uint8_t {
    Immediate,
    Deferred,
};

struct DisplayViewportConfig {
    /* Empty size uses the whole BSP panel. Coordinates are panel-native. */
    bsp_rect_t output_area = {};

    /* Empty size is derived from output_area and rotation at scale 1. */
    bsp_size_t logical_size = {};

    bsp_rotation_t rotation = BSP_ROTATION_0;
};

struct DisplayBufferConfig {
    /* Zero values select DisplayManager defaults. */
    int lines = 0;
    int count = 0;
    uint32_t memory_caps = 0;
};

struct DisplayManagerConfig {
    DisplayViewportConfig viewport;
    DisplayPresentMode present_mode = DisplayPresentMode::Immediate;
    DisplayBufferConfig buffer;
    bool make_default = true;
};

struct DisplayManagerContext;

class DisplayManager {
public:
    DisplayManager() = default;

    DisplayManager(const DisplayManager &) = delete;
    DisplayManager &operator=(const DisplayManager &) = delete;

    esp_err_t create_display(const DisplayManagerConfig &config,
                             lv_display_t **out_display);

    /* Call these from the LVGL context or while holding the LVGL lock. */
    esp_err_t set_rotation(lv_display_t *display, bsp_rotation_t rotation);
    esp_err_t set_epd_mode(lv_display_t *display, bsp_epd_mode_t mode,
                           bool once);
    /* Direct displays still render into the physical framebuffer while hidden. */
    esp_err_t set_visible(lv_display_t *display, bool visible);

    /* framebuffer_index is ignored when the BSP has no framebuffers. */
    esp_err_t compose(lv_display_t *display, int framebuffer_index);
    esp_err_t present(int framebuffer_index);

private:
    static void flush_cb(lv_display_t *display, const lv_area_t *area,
                         uint8_t *pixels);
    DisplayManagerContext *context_for(lv_display_t *display) const {
        if (!display) return nullptr;
        return static_cast<DisplayManagerContext *>(
            lv_display_get_user_data(display));
    }

};

extern DisplayManager display_manager;
