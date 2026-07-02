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
#include "sim_harness.h"

#include <SDL2/SDL.h>
#include <jpeglib.h>

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

/* Initial host-view rotation (0/90/180/270). The build overrides it — the
 * simulator CMakeLists passes SDL_PANEL_DEFAULT_ROTATION; the r/l keys change it
 * from here at runtime. */
#ifndef SDL_PANEL_DEFAULT_ROTATION
#define SDL_PANEL_DEFAULT_ROTATION 0
#endif

static SDL_Window   *s_window;
static SDL_Renderer *s_renderer;
static SDL_Texture  *s_texture;

static bsp_display_type_t s_type = BSP_DISPLAY_TYPE_NONE;
static int                s_panel_w;
static int                s_panel_h;
static int                s_scale_div = 1;
static int                s_window_rotation = SDL_PANEL_DEFAULT_ROTATION;   /* host-view rotation only (r/l keys); panel + touch coords are unaffected */
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
static bsp_touch_t   s_touch;

/* Touch snapshot in PANEL coordinates. Setters (pump_input's mouse read, the
 * harness's inject_*) mutate s_touch_pts under s_touch_mtx and fire
 * bsp_touch_notify() -- the sim's stand-in for the device INT edge. */
#define SDL_PANEL_MAX_TOUCH 10
static SDL_mutex *s_touch_mtx;
static struct {
    bool active;
    int  id;
    int  x, y;
} s_touch_pts[SDL_PANEL_MAX_TOUCH];
static bool s_touch_dirty;

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

/* Rotate the host view by `delta` degrees (r/l keys), swapping the window's
 * aspect for 90/270. This only changes how the glass is presented on screen and
 * how the mouse is mapped back — the panel buffers and touch coordinate space are
 * untouched. */
static void rotate_window(int delta) {
    if (!s_window) return;
    s_window_rotation = (s_window_rotation + delta + 360) % 360;
    int base_w = s_panel_w / s_scale_div;
    int base_h = s_panel_h / s_scale_div;
    bool swap = (s_window_rotation == 90 || s_window_rotation == 270);
    SDL_SetWindowSize(s_window, swap ? base_h : base_w, swap ? base_w : base_h);
    s_dirty = true;   /* re-present at the new size/orientation */
}

/* Drain the SDL event queue: window close quits; r/l rotate the host view; ESC
 * quits. Touch is sampled from the mouse state in pump_input, not from events. */
static void pump_events(void) {
    SDL_Event ev;
    while (SDL_PollEvent(&ev)) {
        switch (ev.type) {
        case SDL_QUIT:
            exit(0);
        case SDL_KEYDOWN:
            switch (ev.key.keysym.sym) {
            case SDLK_ESCAPE: exit(0);
            case SDLK_r: rotate_window( 90); break;
            case SDLK_l: rotate_window(-90); break;
            default: break;
            }
            break;
        case SDL_WINDOWEVENT:
            if (ev.window.event == SDL_WINDOWEVENT_SIZE_CHANGED ||
                ev.window.event == SDL_WINDOWEVENT_EXPOSED) {
                s_dirty = true;   /* re-letterbox at the new drawable size */
            }
            break;
        default: break;
        }
    }
}

/* Aspect-preserving fit of the (rotated) content into a vw×vh viewport, centered.
 * Content footprint is the panel size, swapped for 90/270. Outputs the on-screen
 * rect; works in whatever units the viewport is given (drawable px or window pts). */
static void content_fit(int vw, int vh, int *ox, int *oy, int *ow, int *oh) {
    bool swap = (s_window_rotation == 90 || s_window_rotation == 270);
    double cw = swap ? s_panel_h : s_panel_w;
    double ch = swap ? s_panel_w : s_panel_h;
    double scale = ((double)vw / cw < (double)vh / ch) ? (double)vw / cw : (double)vh / ch;
    int fw = (int)(cw * scale + 0.5);
    int fh = (int)(ch * scale + 0.5);
    *ow = fw; *oh = fh;
    *ox = (vw - fw) / 2;
    *oy = (vh - fh) / 2;
}

/* Map on-screen window coordinates to panel pixels, undoing the aspect-preserving
 * letterbox (any window size), the host-view rotation, and clamping to the panel. */
static void window_to_panel(int wx, int wy, int *px, int *py) {
    int ww = s_panel_w / s_scale_div, wh = s_panel_h / s_scale_div;
    if (s_window) SDL_GetWindowSize(s_window, &ww, &wh);
    if (ww <= 0) ww = 1;
    if (wh <= 0) wh = 1;
    int cx, cy, cw, ch;
    content_fit(ww, wh, &cx, &cy, &cw, &ch);
    if (cw <= 0) cw = 1;
    if (ch <= 0) ch = 1;
    double nx = (double)(wx - cx) / cw;   /* normalized within the displayed content */
    double ny = (double)(wy - cy) / ch;
    if (nx < 0.0) nx = 0.0; else if (nx > 1.0) nx = 1.0;
    if (ny < 0.0) ny = 0.0; else if (ny > 1.0) ny = 1.0;
    double fx, fy;                         /* normalized panel coords */
    switch (s_window_rotation) {
        case 90:  fx = ny;       fy = 1.0 - nx; break;
        case 180: fx = 1.0 - nx; fy = 1.0 - ny; break;
        case 270: fx = 1.0 - ny; fy = nx;       break;
        default:  fx = nx;       fy = ny;       break;
    }
    int x = (int)(fx * s_panel_w);
    int y = (int)(fy * s_panel_h);
    if (x < 0) x = 0; else if (x > s_panel_w - 1) x = s_panel_w - 1;
    if (y < 0) y = 0; else if (y > s_panel_h - 1) y = s_panel_h - 1;
    *px = x;
    *py = y;
}

/* Only notify on actual state changes: pump_input calls this every frame
 * regardless, and re-firing on unchanged state would busy-wake the reader. */
static void touch_set_point(int id, bool pressed, int x, int y) {
    bool changed = false;
    if (s_touch_mtx) SDL_LockMutex(s_touch_mtx);
    int found = -1, free_slot = -1;
    for (int i = 0; i < SDL_PANEL_MAX_TOUCH; i++) {
        if (s_touch_pts[i].active && s_touch_pts[i].id == id) { found = i; break; }
        if (free_slot < 0 && !s_touch_pts[i].active) free_slot = i;
    }
    if (pressed) {
        int slot = found >= 0 ? found : free_slot;
        if (slot >= 0) {
            if (!s_touch_pts[slot].active ||
                s_touch_pts[slot].x != x || s_touch_pts[slot].y != y ||
                s_touch_pts[slot].id != id) {
                s_touch_pts[slot].active = true;
                s_touch_pts[slot].id = id;
                s_touch_pts[slot].x = x;
                s_touch_pts[slot].y = y;
                changed = true;
            }
        }
    } else if (found >= 0) {
        s_touch_pts[found].active = false;
        changed = true;
    }
    if (changed) s_touch_dirty = true;
    if (s_touch_mtx) SDL_UnlockMutex(s_touch_mtx);

    if (changed) bsp_touch_notify();
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
static esp_err_t display_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                     bsp_rotation_t rotation) {
    (void)self;
    const bool has_fb = (s_type == BSP_DISPLAY_TYPE_MIPI_DSI || s_type == BSP_DISPLAY_TYPE_RGB);
    uint8_t *target = has_fb ? s_fb[0] : s_glass;
    if (!target) return ESP_ERR_INVALID_STATE;
    if (rotation == BSP_ROTATION_0) blit_rect(target, area, pixels);
    else bsp_blit_rotated(target, s_panel_w, s_bpp, area, pixels, rotation);
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

static esp_err_t display_draw_bitmap_epd(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                         bsp_rotation_t rotation) {
    (void)self;
    if (!s_gram) return ESP_ERR_INVALID_STATE;
    if (rotation == BSP_ROTATION_0) blit_rect(s_gram, area, pixels);
    else bsp_blit_rotated(s_gram, s_panel_w, s_bpp, area, pixels, rotation);
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

/* Blank the panel to white: GRAM + glass, like the device's clear waveform. */
static esp_err_t display_clear_epd(bsp_display_t *self) {
    (void)self;
    if (!s_glass || !s_gram) return ESP_ERR_INVALID_STATE;
    size_t bytes = (size_t)s_panel_w * s_panel_h * s_bpp;
    memset(s_gram, 0xFF, bytes);
    memset(s_glass, 0xFF, bytes);
    s_dirty = true;
    return ESP_OK;
}

/* MARK: bsp_touch vtable */

static esp_err_t touch_poll(bsp_touch_t *self,
                            bsp_touch_raw_point_t *out, uint8_t max,
                            uint8_t *count, bool *keep_polling) {
    (void)self;
    *keep_polling = false;

    uint8_t n = 0;
    if (s_touch_mtx) SDL_LockMutex(s_touch_mtx);
    s_touch_dirty = false;
    for (int i = 0; i < SDL_PANEL_MAX_TOUCH && n < max; i++) {
        if (!s_touch_pts[i].active) continue;
        out[n].x  = (uint16_t)s_touch_pts[i].x;
        out[n].y  = (uint16_t)s_touch_pts[i].y;
        out[n].id = (uint8_t)s_touch_pts[i].id;
        n++;
    }
    if (s_touch_mtx) SDL_UnlockMutex(s_touch_mtx);
    *count = n;
    return ESP_OK;
}

static esp_err_t touch_deinit(bsp_touch_t *self) {
    (void)self;
    return ESP_OK;
}

/* MARK: sim harness capture callback */

/* Encode the current on-glass image to a JPEG at path (registered with the sim
 * harness in sdl_panel_create). The panel format is already ours, so the encoder
 * lives here rather than in the harness — that keeps the harness BSP-agnostic.
 * The snapshot is a plain memory read, so this is safe to call from the harness's
 * interpreter thread concurrently with the main-thread present. */
static void make_parent_dirs(const char *path) {
    char buf[1024];
    size_t n = strlen(path);
    if (n == 0 || n >= sizeof(buf)) return;
    memcpy(buf, path, n + 1);
    for (char *p = buf + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            mkdir(buf, 0755);
            *p = '/';
        }
    }
}

/* Convert one panel pixel (by linear index) from the on-glass format to RGB24. */
static void capture_pixel_rgb(size_t s_idx, uint8_t *dst) {
    switch (s_format) {
    case BSP_PIXEL_FORMAT_RGB888: {   /* framebuffer holds B,G,R (LVGL native) */
        const uint8_t *p = s_present_src + s_idx * 3;
        dst[0] = p[2]; dst[1] = p[1]; dst[2] = p[0];
        break;
    }
    case BSP_PIXEL_FORMAT_L8: {
        uint8_t g = s_present_src[s_idx];
        dst[0] = dst[1] = dst[2] = g;
        break;
    }
    case BSP_PIXEL_FORMAT_RGB565:
    default: {
        uint16_t v = ((const uint16_t *)s_present_src)[s_idx];
        uint8_t r5 = (v >> 11) & 0x1F, g6 = (v >> 5) & 0x3F, b5 = v & 0x1F;
        dst[0] = (uint8_t)((r5 << 3) | (r5 >> 2));
        dst[1] = (uint8_t)((g6 << 2) | (g6 >> 4));
        dst[2] = (uint8_t)((b5 << 3) | (b5 >> 2));
        break;
    }
    }
}

static bool sdl_panel_capture(const char *path) {
    if (!s_present_src) return false;

    make_parent_dirs(path);
    FILE *f = fopen(path, "wb");
    if (!f) {
        fprintf(stderr, "[sim] capture: cannot open %s\n", path);
        return false;
    }

    /* Apply the host-view rotation so the saved image is in the orientation the
     * window shows (CW by s_window_rotation). Headless never runs the r/l keys, so
     * this is exactly the build-time SDL_PANEL_DEFAULT_ROTATION — deterministic. */
    const int  rot    = s_window_rotation;
    const bool swap   = (rot == 90 || rot == 270);
    const int  out_w  = swap ? s_panel_h : s_panel_w;
    const int  out_h  = swap ? s_panel_w : s_panel_h;

    uint8_t *row = malloc((size_t)out_w * 3);
    if (!row) { fclose(f); return false; }

    struct jpeg_compress_struct cinfo;
    struct jpeg_error_mgr jerr;
    cinfo.err = jpeg_std_error(&jerr);
    jpeg_create_compress(&cinfo);
    jpeg_stdio_dest(&cinfo, f);
    cinfo.image_width = (JDIMENSION)out_w;
    cinfo.image_height = (JDIMENSION)out_h;
    cinfo.input_components = 3;
    cinfo.in_color_space = JCS_RGB;
    jpeg_set_defaults(&cinfo);
    jpeg_set_quality(&cinfo, 90, TRUE);
    jpeg_start_compress(&cinfo, TRUE);

    for (int oy = 0; oy < out_h; oy++) {
        for (int ox = 0; ox < out_w; ox++) {
            int sx, sy;   /* source (panel-native) coords for output (ox, oy) */
            switch (rot) {
                case 90:  sx = oy;                 sy = s_panel_h - 1 - ox; break;
                case 270: sx = s_panel_w - 1 - oy; sy = ox;                 break;
                case 180: sx = s_panel_w - 1 - ox; sy = s_panel_h - 1 - oy; break;
                default:  sx = ox;                 sy = oy;                 break;
            }
            capture_pixel_rgb((size_t)sy * s_panel_w + sx, row + (size_t)ox * 3);
        }
        JSAMPROW rp = row;
        jpeg_write_scanlines(&cinfo, &rp, 1);
    }

    jpeg_finish_compress(&cinfo);
    jpeg_destroy_compress(&cinfo);
    free(row);
    fclose(f);
    fprintf(stderr, "[sim] captured %s (%dx%d)\n", path, out_w, out_h);
    return true;
}

/* MARK: lifecycle */

esp_err_t sdl_panel_create(const sdl_panel_config_t *config,
                           bsp_display_t **out_display,
                           bsp_touch_t **out_touch) {
    if (!config || !out_display) return ESP_ERR_INVALID_ARG;
    if (s_present_src || s_window) {  /* already created — single window per process */
        *out_display = &s_display;
        if (out_touch) *out_touch = &s_touch;
        return ESP_OK;
    }

    s_type      = config->type;
    s_panel_w   = config->size.width;
    s_panel_h   = config->size.height;
    s_scale_div = config->scale_div > 0 ? config->scale_div : 1;
    s_bpp       = bsp_pixel_format_bytes(config->format);
    s_format    = config->format;
    s_headless  = getenv("SIMULATOR_HEADLESS") != NULL;
    s_touch_mtx = SDL_CreateMutex();  /* guards the touch snapshot (cross-thread) */

    SDL_SetMainReady();
    /* Headless still needs the SDL timer (LVGL's tick source is SDL_GetTicks)
     * but no video subsystem — no window pops up and no display is required. */
    Uint32 init_flags = s_headless ? SDL_INIT_TIMER : SDL_INIT_VIDEO;
    if (SDL_Init(init_flags) != 0) {
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return ESP_FAIL;
    }

    if (!s_headless) {
        bool rot_swap = (s_window_rotation == 90 || s_window_rotation == 270);
        s_window = SDL_CreateWindow(config->title ? config->title : "BSP Simulator",
                                    SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED,
                                    (rot_swap ? s_panel_h : s_panel_w) / s_scale_div,
                                    (rot_swap ? s_panel_w : s_panel_h) / s_scale_div,
                                    SDL_WINDOW_SHOWN | SDL_WINDOW_ALLOW_HIGHDPI |
                                    SDL_WINDOW_RESIZABLE);
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
    s_display.clear            = NULL;

    switch (config->type) {
    case BSP_DISPLAY_TYPE_RGB:
    case BSP_DISPLAY_TYPE_MIPI_DSI: {
        int fb_num = config->framebuffer.fb_num ? config->framebuffer.fb_num : 1;
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
        s_display.clear        = display_clear_epd;
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

    s_touch.poll       = touch_poll;
    s_touch.deinit     = touch_deinit;
    s_touch.int_io     = -1;   /* no ISR; touch_set_point fires bsp_touch_notify() */
    s_touch.width      = (uint16_t)s_panel_w;
    s_touch.height     = (uint16_t)s_panel_h;
    s_touch.max_points = SDL_PANEL_MAX_TOUCH;

    /* Self-register with the sim harness: it drives these without ever knowing
     * about sdl_panel (input injection + frame capture are this backend's job). */
    sim_harness_set_input_callback(sdl_panel_inject_down, sdl_panel_inject_up);
    sim_harness_set_capture_callback(sdl_panel_capture);

    if (!s_headless) fprintf(stderr, "[sim] keys: r/l rotate view, ESC quit\n");

    *out_display = &s_display;
    if (out_touch) *out_touch = &s_touch;
    return ESP_OK;
}

void sdl_panel_present(void) {
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

    SDL_RenderClear(s_renderer);   /* paint the letterbox margins */
    int dw, dh;
    SDL_GetRendererOutputSize(s_renderer, &dw, &dh);
    int fx, fy, fw, fh;            /* upright content rect, aspect-preserved */
    content_fit(dw, dh, &fx, &fy, &fw, &fh);
    SDL_Rect  dst;
    SDL_Point center;
    if (s_window_rotation == 90 || s_window_rotation == 270) {
        /* The texture is panel-oriented; rotating about its center swaps the
         * footprint, so place an fh×fw rect centered on the content center. */
        int ccx = fx + fw / 2, ccy = fy + fh / 2;
        dst    = (SDL_Rect){ ccx - fh / 2, ccy - fw / 2, fh, fw };
        center = (SDL_Point){ fh / 2, fw / 2 };
    } else {
        dst    = (SDL_Rect){ fx, fy, fw, fh };
        center = (SDL_Point){ fw / 2, fh / 2 };
    }
    SDL_RenderCopyEx(s_renderer, s_texture, NULL, &dst,
                     (double)s_window_rotation, &center, SDL_FLIP_NONE);
    SDL_RenderPresent(s_renderer);
}

void sdl_panel_pump_input(void) {
    /* Main thread only. Drain events (so a window close quits) and, when there is
     * a window, sample the mouse into the id-0 snapshot slot the background
     * touch_read consumes. Headless touch comes from sdl_panel_inject_* instead. */
    if (s_headless) return;
    pump_events();
    int wx, wy;
    Uint32 buttons = SDL_GetMouseState(&wx, &wy);
    if (buttons & SDL_BUTTON(SDL_BUTTON_LEFT)) {
        int px, py;
        window_to_panel(wx, wy, &px, &py);
        touch_set_point(0, true, px, py);
    } else {
        touch_set_point(0, false, 0, 0);
    }
}

void sdl_panel_inject_down(int id, int x, int y) {
    touch_set_point(id, true, x, y);
}

void sdl_panel_inject_up(int id) {
    touch_set_point(id, false, 0, 0);
}

const void *sdl_panel_snapshot(int *width, int *height, bsp_pixel_format_t *format) {
    if (!s_present_src) return NULL;   /* not created yet */
    if (width)  *width  = s_panel_w;
    if (height) *height = s_panel_h;
    if (format) *format = s_format;
    return s_present_src;
}
