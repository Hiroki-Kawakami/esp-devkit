/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal display-driver interface. A driver allocates a struct whose FIRST
 * member is a bsp_display_t (struct-inheritance vtable), fills the function
 * pointers, and returns &state->base from its *_create(). The board then calls
 * through the members directly (single indirection: disp->op(disp, ...)).
 *
 * The portable base contract is draw_bitmap (blit a rectangle of pixels).
 * Host-side framebuffers (get_framebuffers + flush) and a controllable
 * backlight (set_brightness) are optional fast paths: a driver leaves the
 * corresponding pointer NULL when the panel has no such capability (e.g. an EPD
 * has no host framebuffer and no backlight). This keeps the seam usable for
 * non-MIPI panels (SPI-with-GRAM, EPD) without baking the framebuffer-swap
 * model into the contract.
 *
 * EPD panels add two more optional ops (NULL on non-EPD): set_epd_mode sets the
 * persistent waveform mode that draw_bitmap consults — when it is
 * BSP_EPD_MODE_NONE a draw only updates GRAM and leaves the glass unchanged;
 * otherwise the drawn region is pushed to the panel in that mode. refresh pushes
 * the latest GRAM contents to the panel with an explicit one-shot mode (it does
 * not change the persistent mode), e.g. a full GC16 repaint to clear ghosting.
 */

#pragma once
#include "bsp_types.h"

typedef struct bsp_display bsp_display_t;

struct bsp_display {
    /* descriptor — filled by the driver */
    bsp_display_type_t  type;
    bsp_size_t          size;
    bsp_pixel_format_t  format;

    /* portable base contract (always non-NULL) */
    esp_err_t (*draw_bitmap)(bsp_display_t *self, bsp_rect_t area, const void *pixels);
    esp_err_t (*deinit)(bsp_display_t *self);

    /* backlight — NULL when the panel has no controllable backlight */
    esp_err_t (*set_brightness)(bsp_display_t *self, int brightness);

    /* host-side framebuffer fast path — NULL when the panel has no host FB */
    void   ** (*get_framebuffers)(bsp_display_t *self);
    esp_err_t (*flush)(bsp_display_t *self, int fb_index);

    /* EPD refresh control — NULL on non-EPD panels */
    esp_err_t (*set_epd_mode)(bsp_display_t *self, bsp_epd_mode_t mode);
    esp_err_t (*refresh)(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode);
};

/* Register the active display with the common layer (src/bsp_display.c), which
 * implements the model-agnostic public bsp_display_* API on top of it. A board's
 * bsp_init() calls this once after creating its display provider. */
void bsp_display_set_active(bsp_display_t *display);
