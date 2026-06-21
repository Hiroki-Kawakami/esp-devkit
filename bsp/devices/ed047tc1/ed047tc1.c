/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ED047TC1 grayscale EPD driver -- see ed047tc1.h.
 *
 * MVP scope: full-screen blocking refresh. draw_bitmap updates an L8 GRAM in
 * PSRAM; refresh() replays a time-axis waveform LUT, rebuilding every scanline
 * from GRAM each frame and clocking it out through epd_ll. EPD gate scanning
 * always covers the whole panel, so refresh ignores the dirty rect.
 *
 * Scanline packing: 4 source pixels -> one byte, leftmost pixel in the high
 * 2-bit pair (MSB-first), giving width/4 bytes per line. Uniform frames (every
 * gray drives the same) reuse a single prebuilt line for all rows.
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
    uint8_t       *gram;       /* L8, WIDTH*HEIGHT, PSRAM            */
    uint8_t       *line[2];    /* DMA scanline ping-pong, internal   */
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
 * byte k holds pixels 4k..4k+3 -> width/4 bytes per line. */
static void build_line(const uint8_t *row, uint32_t frame, uint8_t *dst) {
    for (int x = 0; x < ED047TC1_WIDTH; x += 4) {
        int d0 = (frame >> ((row[x + 0] >> 4) * 2)) & 3;
        int d1 = (frame >> ((row[x + 1] >> 4) * 2)) & 3;
        int d2 = (frame >> ((row[x + 2] >> 4) * 2)) & 3;
        int d3 = (frame >> ((row[x + 3] >> 4) * 2)) & 3;
        dst[x >> 2] = (uint8_t)((d0 << 6) | (d1 << 4) | (d2 << 2) | d3);
    }
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

static void refresh_lut(ed047tc1_t *s, const uint32_t *lut, size_t steps) {
    power_on(s);
    for (size_t f = 0; f < steps; f++) {
        uint32_t frame = lut[f];
        uint8_t uni_byte;
        bool uniform = frame_uniform(frame, &uni_byte);
        if (uniform) memset(s->line[0], uni_byte, ED047TC1_LINE_BYTES);

        epd_ll_frame_begin();
        for (int y = 0; y < ED047TC1_HEIGHT; y++) {
            uint8_t *buf;
            if (uniform) {
                buf = s->line[0];   /* same constant line for every row */
            } else {
                buf = s->line[y & 1];
                build_line(s->gram + (size_t)y * ED047TC1_WIDTH, frame, buf);
            }
            epd_ll_write_line(buf);
        }
        epd_ll_frame_end();
    }
    power_off(s);
}

// MARK: - vtable

static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels) {
    ed047tc1_t *s = (ed047tc1_t *)self;
    const uint8_t *src = pixels;
    int x0 = area.origin.x, y0 = area.origin.y;
    int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > ED047TC1_WIDTH || y0 + h > ED047TC1_HEIGHT) {
        return ESP_ERR_INVALID_ARG;
    }
    for (int r = 0; r < h; r++) {
        memcpy(s->gram + (size_t)(y0 + r) * ED047TC1_WIDTH + x0,
               src + (size_t)r * w, w);
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
    switch (mode) {
        case BSP_EPD_MODE_FAST:
            refresh_lut(s, ed047tc1_lut_fast, ED047TC1_LUT_FAST_STEPS);
            break;
        case BSP_EPD_MODE_QUALITY:
            refresh_lut(s, ed047tc1_lut_quality, ED047TC1_LUT_QUALITY_STEPS);
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
    free(s->line[0]);
    free(s->line[1]);
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

    s->gram = heap_caps_malloc((size_t)ED047TC1_WIDTH * ED047TC1_HEIGHT, MALLOC_CAP_SPIRAM);
    if (!s->gram) { free(s); return ESP_ERR_NO_MEM; }
    memset(s->gram, 0xFF, (size_t)ED047TC1_WIDTH * ED047TC1_HEIGHT);   /* white */

    size_t lb = (ED047TC1_LINE_BYTES + ED047TC1_LINE_PADDING + 15) & ~(size_t)15;
    for (int i = 0; i < 2; i++) {
        s->line[i] = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
        if (!s->line[i]) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
        memset(s->line[i], 0, lb);   /* padding region stays zero */
    }

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
     * state. Uniform white clear -> fast (one prebuilt line, no per-row build). */
    refresh_lut(s, ed047tc1_lut_clear, ED047TC1_LUT_CLEAR_STEPS);

    *out_display = &s->base;
    return ESP_OK;
}
