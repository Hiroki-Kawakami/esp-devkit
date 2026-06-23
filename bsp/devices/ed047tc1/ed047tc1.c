/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ED047TC1 grayscale EPD driver -- see ed047tc1.h.
 *
 * Blocking refresh with differential drive. draw_bitmap updates an L8 target
 * GRAM in PSRAM; refresh() replays a time-axis waveform LUT, rebuilding every
 * scanline from GRAM each frame and clocking it out through epd_ll. Only pixels
 * whose target differs from the displayed state (`disp`) are driven; the rest
 * hold. The BSP_EPD_MODE_FULL flag forces every pixel to drive (ghost clear).
 * EPD gate scanning always covers the whole panel, so refresh ignores the dirty
 * rect and compares the full frame each time.
 *
 * Scanline packing: 4 source pixels -> one byte, leftmost pixel in the high
 * 2-bit pair (MSB-first), giving width/4 bytes per line. Rows with no driven
 * pixel reuse a constant all-hold line; uniform frames (every gray drives the
 * same) likewise reuse a single prebuilt line for all rows.
 */

#include "ed047tc1.h"
#include "ed047tc1_param.h"
#include "epd_ll.h"
#include <string.h>
#include "esp_heap_caps.h"
#include "esp_rom_sys.h"
#include "driver/gpio.h"
#include "esp_err.h"

typedef struct {
    bsp_display_t  base;
    uint8_t       *gram;       /* L8 target, WIDTH*HEIGHT, PSRAM     */
    uint8_t       *disp;       /* L8 displayed state, PSRAM          */
    uint8_t       *line[2];    /* DMA scanline ping-pong, internal   */
    uint8_t       *hold_line;  /* constant all-hold (0x00) DMA line  */
    bsp_epd_mode_t mode;       /* persistent draw mode               */
    int            oe_pin;
    int            pwr_pin;
} ed047tc1_t;

static void power_on(ed047tc1_t *s) {
    gpio_set_level(s->oe_pin, 1);  esp_rom_delay_us(100);
    gpio_set_level(s->pwr_pin, 1); esp_rom_delay_us(100);
}

static void power_off(ed047tc1_t *s) {
    gpio_set_level(s->pwr_pin, 0); esp_rom_delay_us(1000);
    gpio_set_level(s->oe_pin, 0);  esp_rom_delay_us(100);
}

/* Build one DMA scanline from a GRAM row (L8) for the given waveform frame.
 * 4 pixels pack into one byte, leftmost pixel in the high 2-bit pair (MSB-first);
 * byte k holds pixels 4k..4k+3 -> width/4 bytes per line. A pixel drives only
 * when its target nibble differs from the displayed one; a NULL disp_row drives
 * every pixel (full refresh). Pixels that hold emit action 0. */
static void build_line(const uint8_t *row, const uint8_t *disp_row,
                       uint32_t frame, uint8_t *dst) {
    for (int x = 0; x < ED047TC1_WIDTH; x += 4) {
        int d[4];
        for (int k = 0; k < 4; k++) {
            int g = row[x + k] >> 4;
            bool drive = !disp_row || (disp_row[x + k] >> 4) != g;
            d[k] = drive ? ((frame >> (g * 2)) & 3) : 0;
        }
        dst[x >> 2] = (uint8_t)((d[0] << 6) | (d[1] << 4) | (d[2] << 2) | d[3]);
    }
}

/* True if any pixel in the row has a target nibble differing from displayed. */
static bool row_changed(const uint8_t *row, const uint8_t *disp_row) {
    for (int x = 0; x < ED047TC1_WIDTH; x++) {
        if ((row[x] >> 4) != (disp_row[x] >> 4)) return true;
    }
    return false;
}

/* A frame is uniform when every gray column decodes to the same 2-bit drive
 * (code c repeated 16x = c * 0x55555555). Then one prebuilt scanline serves all
 * rows -- no per-row packing, no GRAM reads -- which is most of the speed win
 * for clears and the solid frames inside a waveform. */
static bool frame_uniform(uint32_t frame, uint8_t *line_byte) {
    if (frame == 0x00000000u || frame == 0x55555555u || frame == 0xAAAAAAAAu) {
        *line_byte = (uint8_t)(frame & 0xFF);   /* 0x00 / 0x55 / 0xAA */
        return true;
    }
    return false;
}

/* Replay a waveform LUT. In diff mode (full == false) only rows with a changed
 * pixel are driven -- the rest emit a constant all-hold line -- and within a
 * driven row only changed pixels move; the uniform fast path stays off because
 * held pixels break per-row uniformity. Full mode drives every pixel and keeps
 * the uniform fast path (one prebuilt line for uniform frames). The displayed
 * state is committed to match the target after the scan. */
static void refresh_lut(ed047tc1_t *s, const uint32_t *lut, size_t steps, bool full) {
    bool row_drv[ED047TC1_HEIGHT];
    bool any = false;
    for (int y = 0; y < ED047TC1_HEIGHT; y++) {
        bool d = full || row_changed(s->gram + (size_t)y * ED047TC1_WIDTH,
                                     s->disp + (size_t)y * ED047TC1_WIDTH);
        row_drv[y] = d;
        any |= d;
    }
    if (!any) return;   /* nothing changed: skip power-up and the whole scan */

    power_on(s);
    for (size_t f = 0; f < steps; f++) {
        uint32_t frame = lut[f];
        uint8_t uni_byte;
        bool uniform = full && frame_uniform(frame, &uni_byte);
        if (uniform) memset(s->line[0], uni_byte, ED047TC1_LINE_BYTES);

        epd_ll_frame_begin();
        for (int y = 0; y < ED047TC1_HEIGHT; y++) {
            uint8_t *buf;
            if (!row_drv[y]) {
                buf = s->hold_line;   /* whole row holds */
            } else if (uniform) {
                buf = s->line[0];     /* same constant line for every row */
            } else {
                buf = s->line[y & 1];
                build_line(s->gram + (size_t)y * ED047TC1_WIDTH,
                           full ? NULL : s->disp + (size_t)y * ED047TC1_WIDTH,
                           frame, buf);
            }
            epd_ll_write_line(buf);
        }
        epd_ll_frame_end();
    }
    power_off(s);

    memcpy(s->disp, s->gram, (size_t)ED047TC1_WIDTH * ED047TC1_HEIGHT);
}

// MARK: - vtable

static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    ed047tc1_t *s = (ed047tc1_t *)self;
    const uint8_t *src = pixels;
    int x0 = area.origin.x, y0 = area.origin.y;
    int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > ED047TC1_WIDTH || y0 + h > ED047TC1_HEIGHT) {
        return ESP_ERR_INVALID_ARG;
    }
    if (rotation == BSP_ROTATION_0) {
        for (int r = 0; r < h; r++) {
            memcpy(s->gram + (size_t)(y0 + r) * ED047TC1_WIDTH + x0, src + (size_t)r * w, w);
        }
    } else {
        bsp_blit_rotated(s->gram, ED047TC1_WIDTH, 1, area, pixels, rotation);
    }
    if (s->mode != BSP_EPD_MODE_NONE) {
        return self->refresh(self, area, s->mode);
    }
    return ESP_OK;
}

static esp_err_t op_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    ((ed047tc1_t *)self)->mode = mode;
    return ESP_OK;
}

static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)area;   /* gate scan always covers the full panel */
    ed047tc1_t *s = (ed047tc1_t *)self;
    bool full = mode & BSP_EPD_MODE_FULL;
    switch (mode & ~BSP_EPD_MODE_FULL) {
        case BSP_EPD_MODE_FAST:
            refresh_lut(s, ed047tc1_lut_fast, ED047TC1_LUT_FAST_STEPS, full);
            break;
        case BSP_EPD_MODE_QUALITY:
            refresh_lut(s, ed047tc1_lut_quality, ED047TC1_LUT_QUALITY_STEPS, full);
            break;
        default:
            break;
    }
    return ESP_OK;
}

static esp_err_t op_deinit(bsp_display_t *self) {
    ed047tc1_t *s = (ed047tc1_t *)self;
    power_off(s);
    free(s->gram);
    free(s->disp);
    free(s->line[0]);
    free(s->line[1]);
    free(s->hold_line);
    free(s);
    return ESP_OK;
}

// MARK: - init

esp_err_t ed047tc1_epd_create(const ed047tc1_config_t *cfg, bsp_display_t **out_display) {
    ed047tc1_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    s->base.type         = BSP_DISPLAY_TYPE_DIRECT_EPD;
    s->base.size         = (bsp_size_t){ ED047TC1_WIDTH, ED047TC1_HEIGHT };
    s->base.format       = BSP_PIXEL_FORMAT_L8;
    s->base.draw_bitmap  = op_draw_bitmap;
    s->base.deinit       = op_deinit;
    s->base.set_epd_mode = op_set_epd_mode;
    s->base.refresh      = op_refresh;
    s->mode              = BSP_EPD_MODE_NONE;
    s->oe_pin            = cfg->oe_pin;
    s->pwr_pin           = cfg->pwr_pin;

    size_t fb = (size_t)ED047TC1_WIDTH * ED047TC1_HEIGHT;
    s->gram = heap_caps_malloc(fb, MALLOC_CAP_SPIRAM);
    s->disp = heap_caps_malloc(fb, MALLOC_CAP_SPIRAM);
    if (!s->gram || !s->disp) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->gram, 0xFF, fb);   /* white target  */
    memset(s->disp, 0xFF, fb);   /* white on glass after the init clear below */

    size_t lb = (ED047TC1_LINE_BYTES + ED047TC1_LINE_PADDING + 15) & ~(size_t)15;
    for (int i = 0; i < 2; i++) {
        s->line[i] = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
        if (!s->line[i]) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
        memset(s->line[i], 0, lb);   /* padding region stays zero */
    }
    s->hold_line = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
    if (!s->hold_line) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->hold_line, 0, lb);   /* all pixels hold (action 0), padding zero */

    epd_ll_config_t ll = {
        .sph_pin      = cfg->sph_pin,
        .cl_pin       = cfg->cl_pin,
        .ckv_pin      = cfg->ckv_pin,
        .spv_pin      = cfg->spv_pin,
        .le_pin       = cfg->le_pin,
        .dc_dummy_pin = cfg->pwr_pin,   /* PWR lent as the i80 dummy D/C pin */
        .pclk_hz      = ED047TC1_PCLK_HZ,
        .line_bytes   = ED047TC1_LINE_BYTES,
        .line_padding = ED047TC1_LINE_PADDING,
    };
    for (int i = 0; i < 8; i++) ll.data_pins[i] = cfg->data_pins[i];
    ESP_ERROR_CHECK(epd_ll_init(&ll));

    /* Power rails off. Configure AFTER epd_ll_init: PWR was lent as the i80
     * dummy D/C and epd_ll hands it back as an output without setting a level,
     * so we own the level here (and the bus init can't clobber it). */
    gpio_config_t gc = {
        .pin_bit_mask = (1ULL << cfg->oe_pin) | (1ULL << cfg->pwr_pin),
        .mode         = GPIO_MODE_OUTPUT,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    ESP_ERROR_CHECK(gpio_config(&gc));
    gpio_set_level(cfg->oe_pin, 0);
    gpio_set_level(cfg->pwr_pin, 0);

    /* Establish a known white screen so later refreshes start from a clean
     * state. Full clear (drives every pixel) with uniform white frames -> one
     * prebuilt line, no per-row build. */
    refresh_lut(s, ed047tc1_lut_clear, ED047TC1_LUT_CLEAR_STEPS, true);

    *out_display = &s->base;
    return ESP_OK;
}
