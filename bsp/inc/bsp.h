/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic public BSP API.
 */

#pragma once
#include "bsp_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    struct {
        uint8_t task_priority;  /*!< refresh-task priority; 0 -> default (5) */
        int8_t  task_affinity;  /*!< core to pin to (0/1); <0 -> no affinity */
    } epd;
    /* Touch reader task: when task_priority > 0, samples are pushed to
     * bsp_touch_set_event_cb(). Zeroed -> no task (poll via bsp_touch_read). */
    struct {
        uint8_t  task_priority;     /*!< reader-task priority; 0 -> no task */
        int8_t   task_affinity;     /*!< core to pin to (0/1); <0 -> no affinity */
        uint16_t poll_interval_ms;  /*!< reader-task INT-wait fallback; 0 -> default */
    } touch;
} bsp_config_t;

esp_err_t bsp_init(const bsp_config_t *config);
void bsp_restart(void);

// MARK: Display
bsp_display_type_t bsp_display_get_type(void);
bsp_size_t bsp_display_get_size(void);
bsp_pixel_format_t bsp_display_get_pixel_format(void);
void bsp_display_set_brightness(int brightness);
void bsp_display_draw_bitmap(bsp_rect_t area, const void *pixels, bsp_rotation_t rotation);
void *bsp_display_get_frame_buffer(int fb_index);
void bsp_display_flush(int fb_index);

// EPD-only: no-op on non-EPD panels.
void bsp_display_set_epd_mode(bsp_epd_mode_t mode);
void bsp_display_refresh(bsp_rect_t area, bsp_epd_mode_t mode);

// MARK: Touch
int bsp_touch_read(bsp_touch_point_t *points, uint8_t max_points);
void bsp_touch_wait_interrupt(void);

/* Push delivery from the touch reader task (bsp_config.touch): `cb` gets
 * display-space points as each sample arrives (count 0 = all released). Runs off
 * the UI thread, so keep it short and synchronize shared state. NULL unregisters. */
typedef void (*bsp_touch_event_cb_t)(const bsp_touch_point_t *points, int count, void *arg);
void bsp_touch_set_event_cb(bsp_touch_event_cb_t cb, void *arg);

// MARK: SD Card
typedef struct {
    bool    format_if_mount_failed;  /*!< default: false */
    uint8_t max_files;               /*!< open-file limit; 0 -> 5 */
    int     max_freq_khz;            /*!< 0 -> SDMMC_FREQ_HIGHSPEED (40 MHz) */
} bsp_sd_mount_config_t;

esp_err_t bsp_sd_mount(const char *mount_point, const bsp_sd_mount_config_t *config);
esp_err_t bsp_sd_unmount(void);
bool bsp_sd_is_mounted(void);

#ifdef __cplusplus
}
#endif
