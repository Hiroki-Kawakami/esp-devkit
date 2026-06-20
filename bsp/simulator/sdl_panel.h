/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Reusable SDL host backend for the BSP simulator boards. This is the
 * simulator-side analogue of devices/ (reusable chip drivers): it turns an SDL
 * window into a bsp_display provider so every model's simulator board can share
 * the same SDL plumbing (window/texture blit, event pump, present).
 *
 * One backend mimics three panel families through the same bsp_display vtable,
 * selected by sdl_panel_config_t::type:
 *   - MIPI_DSI: host framebuffers (get_framebuffers + flush); flush swaps the
 *     presented buffer with no copy, matching the SoC-framebuffer model.
 *   - SPI:      no host framebuffers; draw_bitmap blits straight to the glass
 *     and the region appears immediately.
 *   - EPD:      draw_bitmap writes GRAM; the glass updates only when the
 *     persistent EPD mode is not NONE, or on an explicit refresh().
 * Only what differs per model — window title, geometry, pixel format, panel
 * type, and the type-specific params below — is passed in via the config; a
 * board (boards/<model>/<model>_sim.c) calls sdl_panel_create() and registers
 * the returned provider with bsp_display_set_active().
 *
 * Touch and the headless sim-harness hooks are added on top of this backend
 * later; this header is intentionally display-only for now.
 */

#pragma once
#include "bsp_types.h"
#include "bsp_display.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    const char         *title;        /*!< window title */
    bsp_display_type_t  type;         /*!< which panel family to mimic */
    bsp_size_t          size;         /*!< native panel geometry (px) */
    int                 scale_div;    /*!< window shown at size / scale_div (>=1) */
    bsp_pixel_format_t  format;       /*!< framebuffer / GRAM pixel format */
    union {
        struct {
            uint8_t        fb_num;    /*!< number of host framebuffers (1 or 2) */
        } mipi_dsi;                   /*!< type == BSP_DISPLAY_TYPE_MIPI_DSI */
        struct {
            bsp_epd_mode_t mode;      /*!< initial persistent EPD mode */
        } epd;                        /*!< type == BSP_DISPLAY_TYPE_*_EPD */
    };
} sdl_panel_config_t;

/* Bring up the SDL window and return a bsp_display provider backed by it.
 * Register it with bsp_display_set_active(). Only one window per process is
 * supported (the host runs a single board), so a second call returns the
 * already-created provider.
 *
 * When the SIMULATOR_HEADLESS env var is set, no window/renderer is created
 * (offscreen buffers only) so the simulator runs without a host display. */
esp_err_t sdl_panel_create(const sdl_panel_config_t *config, bsp_display_t **out_display);

/* Present the most recently drawn/flushed content to the SDL window, and drain
 * SDL events (so a window close quits). Call from the MAIN thread once per loop
 * iteration: draws/flushes/refreshes only mark the frame dirty, so the actual
 * SDL/Cocoa render must happen here on the main thread. No-op when headless or
 * when nothing changed since the last present. */
void sdl_panel_present(void);

/* Read-only view of the current on-glass image, in panel pixel format (used by
 * the sim harness for headless snapshots). Returns NULL before creation. */
const void *sdl_panel_snapshot(int *width, int *height, bsp_pixel_format_t *format);

#ifdef __cplusplus
}
#endif
