/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Shared vocabulary for the BSP and its internal device drivers: geometry
 * primitives, pixel format, esp_err, and the nonnull annotation. This is the
 * one header a device driver needs to speak the BSP's types — it deliberately
 * pulls in nothing heavy (no esp_log / stdio).
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define BSP_NONNULL(...) __attribute__((nonnull(__VA_ARGS__)))

typedef struct { int x, y; } bsp_point_t;
typedef struct { int width, height; } bsp_size_t;

typedef struct { bsp_point_t origin; bsp_size_t size; } bsp_rect_t;
static inline int bsp_rect_width(bsp_rect_t rect) { return rect.size.width; }
static inline int bsp_rect_height(bsp_rect_t rect) { return rect.size.height; }
static inline int bsp_rect_min_x(bsp_rect_t rect) { return rect.origin.x; }
static inline int bsp_rect_min_y(bsp_rect_t rect) { return rect.origin.y; }
static inline int bsp_rect_max_x(bsp_rect_t rect) { return rect.origin.x + rect.size.width; }
static inline int bsp_rect_max_y(bsp_rect_t rect) { return rect.origin.y + rect.size.height; }

typedef enum {
    BSP_DISPLAY_TYPE_NONE,
    BSP_DISPLAY_TYPE_SPI,
    BSP_DISPLAY_TYPE_RGB,          /* RGB parallel (esp_lcd_new_rgb_panel) */
    BSP_DISPLAY_TYPE_MIPI_DSI,
    BSP_DISPLAY_TYPE_SPI_EPD,
    BSP_DISPLAY_TYPE_DIRECT_EPD,
} bsp_display_type_t;

typedef enum {
    BSP_PIXEL_FORMAT_L8,
    BSP_PIXEL_FORMAT_RGB565,
    BSP_PIXEL_FORMAT_RGB888,
} bsp_pixel_format_t;

static inline size_t bsp_pixel_format_bytes(bsp_pixel_format_t format) {
    switch (format) {
        case BSP_PIXEL_FORMAT_L8: return 1;
        case BSP_PIXEL_FORMAT_RGB565: return 2;
        case BSP_PIXEL_FORMAT_RGB888: return 3;
        default: return 0;
    }
}

/* Source orientation for bsp_display_draw_bitmap: the pixels are rotated by this
 * much relative to the panel, and the driver un-rotates them into the (panel-
 * coordinate) destination rect while writing. Values mirror lv_display_rotation_t.
 * For 90/270 the source dimensions are the destination rect's, transposed. */
typedef enum {
    BSP_ROTATION_0,
    BSP_ROTATION_90,
    BSP_ROTATION_180,
    BSP_ROTATION_270,
} bsp_rotation_t;

typedef enum {
    BSP_EPD_MODE_NONE    = 0, /* draw() writes framebuffer only, no panel refresh */
    BSP_EPD_MODE_FAST    = 1, /* 2-level direct update, fast                      */
    BSP_EPD_MODE_QUALITY = 2, /* 16 grayscales, flashing, highest quality         */
    BSP_EPD_MODE_TEXT    = 3, /* 16 grayscales, fewer flashes -- text/UI-tuned    */
    BSP_EPD_MODE_SEED    = 4, /* draw() adopts pixels as already on glass: no drive */
    /* OR-able flag for bsp_display_refresh: drive every pixel of the refreshed
     * area, ignoring the diff against the displayed state (ghost clear). The
     * waveform is still selected by the low bits. Blanking the whole panel is a
     * separate op -- bsp_display_clear(). */
    BSP_EPD_MODE_ALL     = 0x80,
    BSP_EPD_MODE_FAST_ALL    = BSP_EPD_MODE_ALL | BSP_EPD_MODE_FAST,
    BSP_EPD_MODE_QUALITY_ALL = BSP_EPD_MODE_ALL | BSP_EPD_MODE_QUALITY,
    BSP_EPD_MODE_TEXT_ALL    = BSP_EPD_MODE_ALL | BSP_EPD_MODE_TEXT,
} bsp_epd_mode_t;

typedef struct {
    int x, y;
    int id;         /*!< pointer track id from the touch controller */
} bsp_touch_point_t;

/* HotKnot — proximity peer-to-peer over the touch panel. Session events are
 * delivered to a bsp_hotknot_event_cb_t (see bsp.h). */
#define BSP_HOTKNOT_MAX_PAYLOAD 128

typedef enum {
    BSP_HOTKNOT_ROLE_SLAVE  = 0,  /*!< approach as slave  (chip cmd 0x20) */
    BSP_HOTKNOT_ROLE_MASTER = 1,  /*!< approach as master (chip cmd 0x21) */
} bsp_hotknot_role_t;

typedef enum {
    BSP_HOTKNOT_EVENT_PAIRED,    /*!< peer detected; FW load starting       */
    BSP_HOTKNOT_EVENT_READY,     /*!< FW loaded; bsp_hotknot_send now allowed */
    BSP_HOTKNOT_EVENT_RECEIVED,  /*!< data arrived (data/len valid in cb only) */
    BSP_HOTKNOT_EVENT_ERROR,     /*!< session failed (err valid); session over */
} bsp_hotknot_event_type_t;

typedef struct {
    bsp_hotknot_event_type_t type;
    const uint8_t           *data;  /*!< RECEIVED only; valid only during the cb */
    size_t                   len;   /*!< RECEIVED only */
    esp_err_t                err;   /*!< ERROR only */
} bsp_hotknot_event_t;

/* Runs on the touch reader task — keep it short and marshal to the UI thread. */
typedef void (*bsp_hotknot_event_cb_t)(const bsp_hotknot_event_t *ev, void *arg);

typedef struct {
    uint16_t year;     /*!< full year, e.g. 2026 */
    uint8_t  month;    /*!< 1-12 */
    uint8_t  day;      /*!< 1-31 */
    uint8_t  hour, minute, second;
    uint8_t  weekday;  /*!< 0-6, 0 = Sunday */
} bsp_rtc_datetime_t;

typedef enum {
    BSP_POWER_CAP_BATTERY = 1 << 0,  /*!< battery voltage / level readable */
    BSP_POWER_CAP_VBUS    = 1 << 1,  /*!< external supply (VBUS) presence readable */
} bsp_power_caps_t;

typedef enum {
    BSP_POWER_SWITCH_USB5V,  /*!< USB host 5V bus power */
} bsp_power_switch_t;

typedef enum {
    BSP_AUDIO_CAP_PCM       = 1 << 0,  /*!< PCM playback path */
    BSP_AUDIO_CAP_TONE      = 1 << 1,  /*!< tone-only buzzer */
    BSP_AUDIO_CAP_SPEAKER   = 1 << 2,  /*!< speaker route */
    BSP_AUDIO_CAP_HEADPHONE = 1 << 3,  /*!< headphone route with insert detect */
} bsp_audio_caps_t;

typedef enum {
    BSP_AUDIO_SPEAKER_MODE_ON   = 0,  /*!< always on (default) */
    BSP_AUDIO_SPEAKER_MODE_AUTO = 1,  /*!< on only while HP is unplugged (needs CAP_HEADPHONE) */
    BSP_AUDIO_SPEAKER_MODE_OFF  = 2,
} bsp_audio_speaker_mode_t;

/* Who drives the DSP chain (audio_dsp.h) that voices the board's output path. */
typedef enum {
    BSP_AUDIO_DSP_MODE_AUTO    = 0,  /*!< board tuning, re-applied on route changes (default) */
    BSP_AUDIO_DSP_MODE_MANUAL  = 1,  /*!< initialised flat; the app drives bsp_audio_dsp() */
    BSP_AUDIO_DSP_MODE_DISABLE = 2,  /*!< no DSP; volume falls back to the codec (clicky) */
} bsp_audio_dsp_mode_t;

#ifdef __cplusplus
}
#endif
