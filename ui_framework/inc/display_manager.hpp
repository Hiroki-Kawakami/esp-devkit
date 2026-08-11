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
    using TouchCallback = void (*)(const bsp_touch_point_t *points, int count,
                                   void *arg);

    DisplayManager() = default;

    DisplayManager(const DisplayManager &) = delete;
    DisplayManager &operator=(const DisplayManager &) = delete;

    /* Also creates an event-driven pointer indev for this display. Touch hit
     * testing prefers the most recently created visible display. */
    esp_err_t create_display(const DisplayManagerConfig &config,
                             lv_display_t **out_display);

    /* Call these from the LVGL context or while holding the LVGL lock. */
    esp_err_t set_rotation(lv_display_t *display, bsp_rotation_t rotation);
    esp_err_t set_epd_mode(lv_display_t *display, bsp_epd_mode_t mode,
                           bool once);
    /* Direct displays still render into the physical framebuffer while hidden. */
    esp_err_t set_visible(lv_display_t *display, bool visible);

    /* Touches outside every visible display viewport are delivered in raw
     * panel coordinates on the BSP dispatch task. */
    esp_err_t set_outside_touch_callback(TouchCallback callback,
                                         void *arg = nullptr);

    /* framebuffer_index is ignored when the BSP has no framebuffers. */
    esp_err_t compose(lv_display_t *display, int framebuffer_index);
    esp_err_t present(int framebuffer_index);

private:
    static constexpr uint8_t kMaxDisplays = 4;

    static void flush_cb(lv_display_t *display, const lv_area_t *area,
                         uint8_t *pixels);
    static void indev_read_cb(lv_indev_t *indev, lv_indev_data_t *data);
    static void touch_event_cb(const bsp_touch_point_t *points, int count,
                               void *arg);
    static void input_dispatch_cb(void *arg);
    DisplayManagerContext *context_for(lv_display_t *display) const {
        if (!display) return nullptr;
        return static_cast<DisplayManagerContext *>(
            lv_display_get_user_data(display));
    }

    DisplayManagerContext *displays_[kMaxDisplays] = {};
    DisplayManagerContext *active_touch_display_ = nullptr;
    TouchCallback outside_touch_callback_ = nullptr;
    void *outside_touch_arg_ = nullptr;
    void *touch_mutex_ = nullptr;
    uint8_t outside_touch_active_ : 1 = false;
    uint8_t input_dispatch_pending_ : 1 = false;
    uint8_t : 6;
};

extern DisplayManager display_manager;
