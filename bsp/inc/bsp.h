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
esp_err_t bsp_power_off(void);

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

// MARK: RTC
/* External I2C RTC (BM8563 on Paper/PaperS3, RX8130 on some M5Stack). Chip is
 * board-selected at build time; calls return ESP_ERR_INVALID_STATE with no RTC. */
bool bsp_rtc_is_available(void);
esp_err_t bsp_rtc_get_time(bsp_rtc_datetime_t *out) BSP_NONNULL(1);
esp_err_t bsp_rtc_set_time(const bsp_rtc_datetime_t *dt) BSP_NONNULL(1);

/* false when power was lost / the oscillator stopped since the last set_time
 * (chip voltage-low flag); set_time clears it. */
esp_err_t bsp_rtc_time_is_valid(bool *out_valid) BSP_NONNULL(1);

/* Countdown: after `seconds` the chip asserts INT and latches the expiry flag,
 * both surviving deep sleep; repeat = auto-reload (interval) vs one-shot.
 * Resolution/max span are chip-specific; an unrepresentable span returns
 * ESP_ERR_INVALID_ARG. */
esp_err_t bsp_rtc_timer_start(uint32_t seconds, bool repeat);
esp_err_t bsp_rtc_timer_stop(void);
esp_err_t bsp_rtc_timer_is_expired(bool *out_expired) BSP_NONNULL(1);
esp_err_t bsp_rtc_timer_clear(void);

/* INT-line callback; needs a board-wired INT gpio, runs ISR-deferred, NULL
 * unregisters. For deep-sleep wakeup use esp_sleep on the INT gpio instead. */
typedef void (*bsp_rtc_int_cb_t)(void *arg);
esp_err_t bsp_rtc_set_int_cb(bsp_rtc_int_cb_t cb, void *arg);

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
