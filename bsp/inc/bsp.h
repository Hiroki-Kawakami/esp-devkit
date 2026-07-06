/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Model-agnostic public BSP API.
 */

#pragma once
#include "bsp_types.h"
#include "audio_dsp.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    struct {
        uint8_t            fb_num;        /*!< framebuffer count; 0 -> board default */
        bsp_pixel_format_t pixel_format;  /*!< 0 -> board default */
    } display;
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
    struct {
        bsp_audio_dsp_mode_t dsp_mode;          /*!< who voices the DSP chain */
        bsp_audio_speaker_mode_t speaker_mode;  /*!< speaker route policy */
    } audio;
} bsp_config_t;

esp_err_t bsp_init(const bsp_config_t *config);

// MARK: Power
/* Power controls (always available). off cuts VSYS and returns ESP_FAIL when
 * external power holds the rail up; restart is a soft reboot (esp_restart);
 * reset is a hardware power-cycle (e.g. via the RTC countdown). */
esp_err_t bsp_power_off(void);
void      bsp_power_restart(void);
esp_err_t bsp_power_hw_reset(void);

/* Toggle a board power switch; unsupported switches return ESP_ERR_NOT_SUPPORTED. */
esp_err_t bsp_power_set_switch(bsp_power_switch_t sw, bool on);

/* Battery / external-supply sensing (board-provided provider). caps == 0 when
 * no provider is registered; calls outside the caps -> ESP_ERR_NOT_SUPPORTED.
 * level is a linear 0..100 map of the terminal voltage between the board's
 * empty/full endpoints -- a coarse gauge, not a true state-of-charge. */
uint32_t  bsp_power_get_caps(void);
esp_err_t bsp_power_get_battery_voltage(uint32_t *out_mv) BSP_NONNULL(1);
esp_err_t bsp_power_get_battery_level(uint8_t *out_percent) BSP_NONNULL(1);
bool      bsp_power_vbus_present(void);

// MARK: Display
bsp_display_type_t bsp_display_get_type(void);
bsp_size_t bsp_display_get_size(void);
bsp_pixel_format_t bsp_display_get_pixel_format(void);
void bsp_display_set_brightness(int brightness);
void bsp_display_draw_bitmap(bsp_rect_t area, const void *pixels, bsp_rotation_t rotation);
void *bsp_display_get_frame_buffer(int fb_index);
void bsp_display_flush(int fb_index);

// EPD-only: no-op on non-EPD panels. refresh honors `area`; OR
// BSP_EPD_MODE_ALL into the mode to drive every pixel of the area (ghost
// clear). clear blanks the whole panel to white (the known-baseline reset).
// Bring-up does not clear — establish a baseline via clear or SEED-mode draws.
void bsp_display_set_epd_mode(bsp_epd_mode_t mode);
void bsp_display_refresh(bsp_rect_t area, bsp_epd_mode_t mode);
void bsp_display_clear(void);

/* Block until no panel update is in flight — the gate before cutting power. */
void bsp_display_wait_idle(void);

// MARK: Touch
int bsp_touch_read(bsp_touch_point_t *points, uint8_t max_points);
void bsp_touch_wait_interrupt(void);

/* Push delivery from the touch reader task (bsp_config.touch): `cb` gets
 * display-space points as each sample arrives (count 0 = all released). Runs off
 * the UI thread, so keep it short and synchronize shared state. NULL unregisters. */
typedef void (*bsp_touch_event_cb_t)(const bsp_touch_point_t *points, int count, void *arg);
void bsp_touch_set_event_cb(bsp_touch_event_cb_t cb, void *arg);

// MARK: HotKnot
/* Proximity peer-to-peer over the touch panel. Needs the touch reader task;
 * events (PAIRED / READY / RECEIVED / ERROR) arrive on `cb`. Ends must use
 * opposite roles. No provider -> ESP_ERR_NOT_SUPPORTED. Caveats: docs/gotchas.md. */
esp_err_t bsp_hotknot_begin(bsp_hotknot_role_t role, bsp_hotknot_event_cb_t cb, void *arg);
esp_err_t bsp_hotknot_send(const void *data, size_t len, uint32_t timeout_ms);
esp_err_t bsp_hotknot_end(void);

// MARK: RTC
/* External I2C RTC, board-selected at build time; calls return
 * ESP_ERR_INVALID_STATE with no RTC. */
bool bsp_rtc_is_available(void);
esp_err_t bsp_rtc_get_time(bsp_rtc_datetime_t *out) BSP_NONNULL(1);
esp_err_t bsp_rtc_set_time(const bsp_rtc_datetime_t *dt) BSP_NONNULL(1);

/* false when power was lost / the oscillator stopped since the last set_time
 * (chip voltage-low flag); set_time clears it. */
esp_err_t bsp_rtc_time_is_valid(bool *out_valid) BSP_NONNULL(1);

/* Countdown: after `ms` the chip asserts INT + latches the expiry flag (both
 * survive deep sleep); repeat = interval vs one-shot. Resolution/max span are
 * chip-specific; an unrepresentable span returns ESP_ERR_INVALID_ARG. */
esp_err_t bsp_rtc_timer_start(uint32_t ms, bool repeat);
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

// MARK: Buttons
/* Board-provided physical buttons; count == 0 when no provider is registered.
 * Callbacks fire on the input task -- marshal to your UI thread yourself.
 *
 * Click dispatch:
 *   - Only click registered   -> fires on release edge, immediately.
 *   - Double-click registered -> the single click is delayed by interval_ms;
 *                                a second press within the window fires
 *                                double_click and cancels the pending click.
 *   - Long-press fired        -> click / double-click are suppressed.
 * DOWN / UP fire on every debounced edge, unconditionally. */

uint8_t bsp_button_count(void);

typedef void (*bsp_button_cb_t)(uint8_t button_id, void *arg);

void bsp_button_on_down(uint8_t id, bsp_button_cb_t cb, void *arg);
void bsp_button_on_up  (uint8_t id, bsp_button_cb_t cb, void *arg);
void bsp_button_on_click(uint8_t id, bsp_button_cb_t cb, void *arg);
void bsp_button_on_double_click(uint8_t id, uint16_t interval_ms,
                                bsp_button_cb_t cb, void *arg);
void bsp_button_on_long_press(uint8_t id, uint16_t duration_ms,
                              bsp_button_cb_t cb, void *arg);

// MARK: RGB LED
/* Board-provided addressable RGB LED(s); count == 0 when no provider is
 * registered. set_rgb / clear return ESP_ERR_INVALID_STATE in that case and
 * ESP_ERR_INVALID_ARG when index >= count. */
uint8_t bsp_led_count(void);
esp_err_t bsp_led_set_rgb(uint8_t index, uint8_t r, uint8_t g, uint8_t b);
esp_err_t bsp_led_clear(void);

// MARK: Audio
/* Capability-based: calls outside the active provider's caps return
 * ESP_ERR_NOT_SUPPORTED; 0 = this model has no audio. */
uint32_t bsp_audio_get_caps(void);

/* Tone (CAP_TONE or CAP_PCM): the hardware buzzer when the model has one,
 * otherwise synthesized on the PCM path (ESP_ERR_INVALID_STATE while a PCM
 * stream is open — no mixing). duration_ms=0 plays until tone_stop. */
esp_err_t bsp_audio_tone(uint32_t freq_hz, uint32_t duration_ms);
esp_err_t bsp_audio_tone_stop(void);

/* PCM playback (CAP_PCM). Idempotent: open starts the stream, reconfigures a
 * running one when the format differs, and no-ops when it matches; close on a
 * closed stream is a no-op. Streams fade in from silence (click-free);
 * write before open returns ESP_ERR_INVALID_STATE. */
esp_err_t bsp_audio_open(uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels);
esp_err_t bsp_audio_close(void);
/* `data` is filtered in-place by the DSP chain — caller must own the buffer.
 * Blocks while the output is full (the device's natural pacing). */
esp_err_t bsp_audio_write(void *data, size_t len);

/* 0..150, linear-in-dB: 0 = true silence, 100 = unity, >100 = digital boost
 * (SW-gain path only — the HW-codec fallback caps at 100). Click-free via the
 * gain fade. Starts at 0 until first set; callable before open. */
esp_err_t bsp_audio_set_volume(int volume);
int       bsp_audio_get_volume(void);
esp_err_t bsp_audio_set_mute(bool mute);
bool      bsp_audio_get_mute(void);

/* DSP chain handle (NULL in DSP_MODE_DISABLE / no PCM path). set_gain is owned
 * by the volume plumbing, and in AUTO mode route changes overwrite direct
 * edits — set_eq_enabled is the EQ toggle that survives re-voicing. */
audio_dsp_t bsp_audio_dsp(void);
esp_err_t bsp_audio_set_eq_enabled(bool enabled);
bool      bsp_audio_get_eq_enabled(void);

/* Speaker route policy (CAP_SPEAKER; AUTO additionally needs CAP_HEADPHONE). */
esp_err_t bsp_audio_set_speaker_mode(bsp_audio_speaker_mode_t mode);
bsp_audio_speaker_mode_t bsp_audio_get_speaker_mode(void);

/* Headphone detect (CAP_HEADPHONE); the callback fires from an internal
 * poller task (~200 ms), NULL unregisters. */
bool bsp_audio_headphone_inserted(void);
typedef void (*bsp_audio_headphone_cb_t)(bool inserted, void *user);
esp_err_t bsp_audio_set_headphone_callback(bsp_audio_headphone_cb_t cb, void *user);

#ifdef __cplusplus
}
#endif
