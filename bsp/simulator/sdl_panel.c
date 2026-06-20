/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * SDL implementation of the BSP simulator display backend (see sdl_panel.h). It
 * fills a bsp_display_t vtable backed by an SDL window so app code reaches it
 * through the same bsp_display_* API on host and device, and mimics three panel
 * families behind that one vtable (see sdl_panel.h for the model).
 *
 * Presentation model. The on-glass image is whatever s_present_src points at.
 * For MIPI that is one of the host framebuffers the app renders into (flush just
 * repoints it — no copy); for SPI/EPD it is s_glass, into which draw_bitmap /
 * refresh composite. Every mutator only marks s_dirty and the actual SDL render
 * is deferred to sdl_panel_present() on the MAIN thread, because SDL/Cocoa
 * rendering is main-thread-only on macOS and a draw may arrive from a background
 * producer.
 *
 * Only one SDL window per process is supported (the host runs a single board),
 * so all state and the single provider instance are file-static.
 */

#include "sdl_panel.h"

#include <SDL2/SDL.h>

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static SDL_Window   *s_window;
static SDL_Renderer *s_renderer;
static SDL_Texture  *s_texture;

static bsp_display_type_t s_type = BSP_DISPLAY_TYPE_NONE;
static int                s_panel_w;
static int                s_panel_h;
static int                s_scale_div = 1;
static size_t             s_bpp = 2;          /* bytes per pixel of the panel format */
static bsp_pixel_format_t s_format = BSP_PIXEL_FORMAT_RGB565;

#define SDL_PANEL_MAX_FB 3
static uint8_t *s_fb[SDL_PANEL_MAX_FB];       /* MIPI host framebuffers */
static void    *s_fb_ptrs[SDL_PANEL_MAX_FB];
static int      s_fb_num;

static uint8_t *s_glass;   /* on-glass image for SPI/EPD (also EPD composite target) */
static uint8_t *s_gram;    /* EPD: written-but-maybe-unshown image */
static uint8_t *s_rgb;     /* L8 -> RGB24 scratch for SDL upload (L8 panels only) */

/* What present()/snapshot() read: a framebuffer (MIPI) or s_glass (SPI/EPD). */
static const uint8_t *s_present_src;

static bsp_epd_mode_t s_epd_mode = BSP_EPD_MODE_NONE;

/* Headless verification support (driven by the sim harness later). With
 * SIMULATOR_HEADLESS set we skip the SDL window/renderer/texture entirely — no
 * host display, so it runs in non-interactive shells / CI. Buffers are still
 * maintained so a snapshot reflects a finished frame. */
static bool s_headless;

/* Set by any mutator (any thread); the actual SDL render is deferred to
 * sdl_panel_present() on the main thread. */
static bool s_dirty;

static bsp_display_t s_display;

static Uint32 sdl_texture_format(bsp_pixel_format_t fmt) {
    switch (fmt) {
        // RGB888 framebuffers hold LVGL's native byte order (B, G, R), so the
        // texture is BGR24 (memory B,G,R), not RGB24.
        case BSP_PIXEL_FORMAT_RGB888: return SDL_PIXELFORMAT_BGR24;
        // SDL has no 8-bit grayscale streaming format; L8 is expanded to RGB24
        // in present() via s_rgb, so the texture is 24-bit.
        case BSP_PIXEL_FORMAT_L8:     return SDL_PIXELFORMAT_RGB24;
        default:                      return SDL_PIXELFORMAT_RGB565;
    }
}

/* Drain the SDL event queue. Touch is not handled yet, so the only event we act
 * on is window close. */
static void pump_events(void) {
    SDL_Event ev;
    while (SDL_PollEvent(&ev)) {
        if (ev.type == SDL_QUIT) exit(0);
    }
}

/* Blit a rectangle of source pixels into a panel-sized destination buffer at the
 * area's origin. Rows outside the panel are skipped; the caller's area is assumed
 * within the panel horizontally (LVGL clips to the panel). */
static void blit_rect(uint8_t *dst, bsp_rect_t area, const void *pixels) {
    int w = area.size.width, h = area.size.height;
    if (w <= 0 || h <= 0) return;
    size_t row_bytes = (size_t)w * s_bpp;
    const uint8_t *src = pixels;
    for (int row = 0; row < h; row++) {
        int dy = area.origin.y + row;
        if (dy < 0 || dy >= s_panel_h) continue;
        uint8_t *d = dst + ((size_t)dy * s_panel_w + area.origin.x) * s_bpp;
        memcpy(d, src + (size_t)row * row_bytes, row_bytes);
    }
}

/* MARK: bsp_display vtable — base */

static esp_err_t display_deinit(bsp_display_t *self) {
    (void)self;
    return ESP_OK;
}

static esp_err_t display_set_brightness(bsp_display_t *self, int brightness) {
    (void)self;
    (void)brightness;
    return ESP_OK;  /* no backlight on host */
}

/* MARK: bsp_display vtable — SPI / MIPI direct draw */

/* Blit straight to the presented buffer and mark it for present. For SPI that is
 * s_glass; for MIPI it is framebuffer 0 (draw_bitmap is the non-framebuffer
 * fallback path there — the usual MIPI path is flush). */
static esp_err_t display_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels) {
    (void)self;
    uint8_t *target = (s_type == BSP_DISPLAY_TYPE_MIPI_DSI) ? s_fb[0] : s_glass;
    if (!target) return ESP_ERR_INVALID_STATE;
    blit_rect(target, area, pixels);
    s_present_src = target;
    s_dirty = true;
    return ESP_OK;
}

/* MARK: bsp_display vtable — MIPI framebuffers */

static void **display_get_framebuffers(bsp_display_t *self) {
    (void)self;
    return s_fb_ptrs;
}

static esp_err_t display_flush(bsp_display_t *self, int fb_index) {
    (void)self;
    if (fb_index < 0 || fb_index >= s_fb_num) fb_index = 0;
    s_present_src = s_fb[fb_index];   /* no copy — present reads the FB directly */
    s_dirty = true;
    return ESP_OK;
}

/* MARK: bsp_display vtable — EPD */

/* Composite an area of GRAM onto the glass. Phase 1: a straight copy regardless
 * of mode (behaviourally correct — the glass shows the latest content). The mode
 * is kept so Phase 2 can quantize here (FAST = 2-level dither, QUALITY = 16-level
 * + white flash) without touching callers. */
static void epd_composite(bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)mode;
    if (!s_glass || !s_gram) return;
    int w = area.size.width, h = area.size.height;
    if (w <= 0 || h <= 0) return;
    size_t row_bytes = (size_t)w * s_bpp;
    for (int row = 0; row < h; row++) {
        int y = area.origin.y + row;
        if (y < 0 || y >= s_panel_h) continue;
        size_t off = ((size_t)y * s_panel_w + area.origin.x) * s_bpp;
        memcpy(s_glass + off, s_gram + off, row_bytes);
    }
    s_dirty = true;
}

static esp_err_t display_draw_bitmap_epd(bsp_display_t *self, bsp_rect_t area, const void *pixels) {
    (void)self;
    if (!s_gram) return ESP_ERR_INVALID_STATE;
    blit_rect(s_gram, area, pixels);
    if (s_epd_mode != BSP_EPD_MODE_NONE) epd_composite(area, s_epd_mode);
    return ESP_OK;
}

static esp_err_t display_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    (void)self;
    s_epd_mode = mode;
    return ESP_OK;
}

static esp_err_t display_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)self;
    epd_composite(area, mode);
    return ESP_OK;
}

/* MARK: lifecycle */

esp_err_t sdl_panel_create(const sdl_panel_config_t *config, bsp_display_t **out_display) {
    if (!config || !out_display) return ESP_ERR_INVALID_ARG;
    if (s_present_src || s_window) {  /* already created — single window per process */
        *out_display = &s_display;
        return ESP_OK;
    }

    s_type      = config->type;
    s_panel_w   = config->size.width;
    s_panel_h   = config->size.height;
    s_scale_div = config->scale_div > 0 ? config->scale_div : 1;
    s_bpp       = bsp_pixel_format_bytes(config->format);
    s_format    = config->format;
    s_headless  = getenv("SIMULATOR_HEADLESS") != NULL;

    SDL_SetMainReady();
    /* Headless still needs the SDL timer (LVGL's tick source is SDL_GetTicks)
     * but no video subsystem — no window pops up and no display is required. */
    Uint32 init_flags = s_headless ? SDL_INIT_TIMER : SDL_INIT_VIDEO;
    if (SDL_Init(init_flags) != 0) {
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return ESP_FAIL;
    }

    if (!s_headless) {
        s_window = SDL_CreateWindow(config->title ? config->title : "BSP Simulator",
                                    SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
                                    s_panel_w / s_scale_div, s_panel_h / s_scale_div,
                                    SDL_WINDOW_SHOWN | SDL_WINDOW_ALLOW_HIGHDPI);
        s_renderer = SDL_CreateRenderer(s_window, -1,
                                        SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC);
        s_texture = SDL_CreateTexture(s_renderer, sdl_texture_format(config->format),
                                      SDL_TEXTUREACCESS_STREAMING, s_panel_w, s_panel_h);
        if (!s_window || !s_renderer || !s_texture) {
            fprintf(stderr, "SDL init failed: %s\n", SDL_GetError());
            return ESP_FAIL;
        }
        if (s_format == BSP_PIXEL_FORMAT_L8) {
            s_rgb = calloc(1, (size_t)s_panel_w * s_panel_h * 3);
            if (!s_rgb) return ESP_ERR_NO_MEM;
        }
    }

    size_t panel_bytes = (size_t)s_panel_w * s_panel_h * s_bpp;

    /* Allocate only the buffers this panel type uses, and wire the vtable. */
    s_display.size   = config->size;
    s_display.format = config->format;
    s_display.type   = config->type;
    s_display.draw_bitmap      = display_draw_bitmap;
    s_display.deinit           = display_deinit;
    s_display.set_brightness   = NULL;
    s_display.get_framebuffers = NULL;
    s_display.flush            = NULL;
    s_display.set_epd_mode     = NULL;
    s_display.refresh          = NULL;

    switch (config->type) {
    case BSP_DISPLAY_TYPE_MIPI_DSI: {
        int fb_num = config->mipi_dsi.fb_num ? config->mipi_dsi.fb_num : 1;
        if (fb_num > SDL_PANEL_MAX_FB) fb_num = SDL_PANEL_MAX_FB;
        s_fb_num = fb_num;
        for (int i = 0; i < fb_num; i++) {
            s_fb[i] = calloc(1, panel_bytes);
            if (!s_fb[i]) return ESP_ERR_NO_MEM;
            s_fb_ptrs[i] = s_fb[i];
        }
        s_present_src = s_fb[0];
        s_display.set_brightness   = display_set_brightness;
        s_display.get_framebuffers = display_get_framebuffers;
        s_display.flush            = display_flush;
        break;
    }
    case BSP_DISPLAY_TYPE_SPI_EPD:
    case BSP_DISPLAY_TYPE_DIRECT_EPD: {
        s_glass = calloc(1, panel_bytes);
        s_gram  = calloc(1, panel_bytes);
        if (!s_glass || !s_gram) return ESP_ERR_NO_MEM;
        s_epd_mode = config->epd.mode;
        s_present_src = s_glass;
        s_display.draw_bitmap  = display_draw_bitmap_epd;
        s_display.set_epd_mode = display_set_epd_mode;
        s_display.refresh      = display_refresh;
        break;
    }
    case BSP_DISPLAY_TYPE_SPI:
    default: {
        s_glass = calloc(1, panel_bytes);
        if (!s_glass) return ESP_ERR_NO_MEM;
        s_present_src = s_glass;
        s_display.set_brightness = display_set_brightness;
        break;
    }
    }

    if (s_renderer) {
        SDL_SetRenderDrawColor(s_renderer, 0, 0, 0, 0xFF);
        SDL_RenderClear(s_renderer);
        SDL_RenderPresent(s_renderer);
    }

    *out_display = &s_display;
    return ESP_OK;
}

void sdl_panel_present(void) {
    if (!s_headless) pump_events();
    if (s_headless || !s_texture || !s_dirty || !s_present_src) return;
    s_dirty = false;

    if (s_format == BSP_PIXEL_FORMAT_L8) {
        size_t n = (size_t)s_panel_w * s_panel_h;
        for (size_t i = 0; i < n; i++) {
            uint8_t g = s_present_src[i];
            s_rgb[3 * i + 0] = g;
            s_rgb[3 * i + 1] = g;
            s_rgb[3 * i + 2] = g;
        }
        SDL_UpdateTexture(s_texture, NULL, s_rgb, s_panel_w * 3);
    } else {
        SDL_UpdateTexture(s_texture, NULL, s_present_src, s_panel_w * (int)s_bpp);
    }
    SDL_RenderCopy(s_renderer, s_texture, NULL, NULL);
    SDL_RenderPresent(s_renderer);
}

const void *sdl_panel_snapshot(int *width, int *height, bsp_pixel_format_t *format) {
    if (!s_present_src) return NULL;   /* not created yet */
    if (width)  *width  = s_panel_w;
    if (height) *height = s_panel_h;
    if (format) *format = s_format;
    return s_present_src;
}
