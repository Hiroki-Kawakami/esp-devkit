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
