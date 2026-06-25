/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Direct-drive (TCON-less) grayscale EPD driver for ESP32-S3 -- see epd_ll.h.
 *
 * Two concerns live here, both SoC/bus-specific (re-tailored per peripheral, not
 * abstracted): the low-level i80 bus + CKV/SPV/LE scan protocol (file-static, the
 * `bus_*` helpers), and the refresh loop on top of it. The per-frame / per-scanline
 * pixel logic is the panel-agnostic, SoC-free epd_waveform.h (host unit-tested via
 * driver/test/run.sh); this file adds the framebuffer, the i80 scan, and the
 * synchronous power-sequenced refresh around it.
 *
 * Data model. ONE gray framebuffer `gram` (4bpp, 2 px/byte) plus a 1bpp `dirty`
 * mask. draw stamps the new gray and marks dirty only where the target differs
 * from the current nibble (the diff-skip baseline -- the waveform is target-only,
 * so the old gray is never needed once a pixel is drawn). refresh replays the
 * mode's LUT frame-by-frame over the whole panel, packing each scanline through
 * the dirty gate, then clears dirty (the driven pixels are now confirmed).
 *
 * Refresh is SYNCHRONOUS: refresh() drives the whole waveform inline and returns
 * when the panel has settled. Only one refresh runs at a time -- there are no
 * concurrent generations and no background task. (This trades the old
 * transaction-index pipeline for a far simpler, cheaper-per-pixel scan; the win
 * is meant to free CPU so the refresh can later be moved back onto a task that
 * yields to the UI during the bus-bound parts.)
 */

#include "epd_ll.h"
#include "epd_waveform.h"
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_io_i80.h"
#include "driver/gpio.h"
#include "esp_heap_caps.h"
#include "esp_rom_sys.h"
#include "esp_attr.h"
#include "esp_check.h"

/* Diagnostic: per-refresh blit/bus timing summary (total time + blit / dma-wait /
 * tx_color split, us/line). Set to 1 while tuning the refresh path. */
#define EPD_PROFILE 1
#if EPD_PROFILE
#include "esp_timer.h"
#include "esp_log.h"
#endif

static const char *TAG = "epd_ll";

// MARK: - i80 bus + CKV/SPV/LE scan protocol (singleton, internal)

static esp_lcd_i80_bus_handle_t   s_i80_bus;
static esp_lcd_panel_io_handle_t  s_io;
static volatile bool              s_dma_busy;   /* a scanline DMA is in flight */
static int      s_ckv_pin, s_spv_pin, s_le_pin;
static uint16_t s_line_bytes;
static uint8_t  s_line_padding;
#if EPD_PROFILE
static int64_t  s_wait_us, s_tx_us;   /* bus_write_line split: dma-wait vs tx_color */
#endif

static IRAM_ATTR bool on_trans_done(esp_lcd_panel_io_handle_t io,
                                    esp_lcd_panel_io_event_data_t *edata,
                                    void *user_ctx) {
    gpio_set_level(s_ckv_pin, 0);
    gpio_set_level(s_le_pin, 1);
    s_dma_busy = false;
    return false;
}

static esp_err_t bus_init(const epd_ll_config_t *cfg) {
    s_dma_busy     = false;
    s_ckv_pin      = cfg->ckv_pin;
    s_spv_pin      = cfg->spv_pin;
    s_le_pin       = cfg->le_pin;
    s_line_bytes   = cfg->line_bytes;
    s_line_padding = cfg->line_padding;

    /* Bit-banged control lines CKV/SPV (gate) + LE (latch), all idle low. */
    gpio_config_t gc = {
        .pin_bit_mask = (1ULL << cfg->ckv_pin) | (1ULL << cfg->spv_pin) | (1ULL << cfg->le_pin),
        .mode         = GPIO_MODE_OUTPUT,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    ESP_RETURN_ON_ERROR(gpio_config(&gc), TAG, "ctrl gpio");
    gpio_set_level(cfg->ckv_pin, 0);
    gpio_set_level(cfg->spv_pin, 0);
    gpio_set_level(cfg->le_pin, 0);

    esp_lcd_i80_bus_config_t bus_cfg = {
        .dc_gpio_num        = cfg->dc_dummy_pin,
        .wr_gpio_num        = cfg->cl_pin,
        .clk_src            = LCD_CLK_SRC_PLL160M,
        .bus_width          = 8,
        .max_transfer_bytes = cfg->line_bytes + cfg->line_padding,
        .dma_burst_size     = 64,
    };
    for (int i = 0; i < 8; i++) {
        bus_cfg.data_gpio_nums[i] = cfg->data_pins[i];
        gpio_set_direction(cfg->data_pins[i], GPIO_MODE_OUTPUT);
    }
    gpio_set_direction(cfg->cl_pin, GPIO_MODE_OUTPUT);
    gpio_set_direction(cfg->sph_pin, GPIO_MODE_OUTPUT);
    ESP_RETURN_ON_ERROR(esp_lcd_new_i80_bus(&bus_cfg, &s_i80_bus), TAG, "i80 bus");

    /* Reclaim the dummy D/C pin as a plain GPIO output (caller sets its level).
     * gpio_set_direction alone re-routes the pad; gpio_reset_pin would enable the
     * pull-up and briefly drive it -- a stray glitch on the PWR pin. */
    gpio_set_direction(cfg->dc_dummy_pin, GPIO_MODE_OUTPUT);

    esp_lcd_panel_io_i80_config_t io_cfg = {
        .cs_gpio_num         = cfg->sph_pin,
        .pclk_hz             = cfg->pclk_hz,
        .trans_queue_depth   = 4,
        .on_color_trans_done = on_trans_done,
        .lcd_cmd_bits        = 0,
        .lcd_param_bits      = 0,
        .dc_levels = { .dc_idle_level = 0, .dc_cmd_level = 0, .dc_dummy_level = 0, .dc_data_level = 1 },
        .flags     = { .pclk_idle_low = 1 },
    };
    ESP_RETURN_ON_ERROR(esp_lcd_new_panel_io_i80(s_i80_bus, &io_cfg, &s_io), TAG, "i80 io");

    return ESP_OK;
}

/* Pulse SPV/CKV to scan the gate driver back to the top row. */
static void bus_frame_begin(void) {
    const int ckv = s_ckv_pin, spv = s_spv_pin;
    while (s_dma_busy) taskYIELD();
    gpio_set_level(spv, 0); esp_rom_delay_us(1);
    gpio_set_level(ckv, 0); esp_rom_delay_us(3);
    gpio_set_level(ckv, 1); esp_rom_delay_us(1);
    gpio_set_level(spv, 1);
    for (int i = 0; i < 3; i++) {
        esp_rom_delay_us(3);
        gpio_set_level(ckv, 0);
        esp_rom_delay_us(3);
        gpio_set_level(ckv, 1);
    }
}

/* Queue one scanline. Blocks until the previous transfer (and its latch) is done,
 * so the caller can reuse a buffer between calls; the buffer passed here stays in
 * use by DMA until the *next* call (or bus_frame_end), so pipeline with two. */
static void bus_write_line(const uint8_t *data) {
#if EPD_PROFILE
    int64_t w0 = esp_timer_get_time();
#endif
    while (s_dma_busy) taskYIELD();        /* wait previous scanline DMA + its latch */
#if EPD_PROFILE
    int64_t w1 = esp_timer_get_time(); s_wait_us += w1 - w0;
#endif
    s_dma_busy = true;
    gpio_set_level(s_le_pin, 0);           /* close the latch before shifting */
    gpio_set_level(s_ckv_pin, 1);          /* CKV high for this line */
    esp_lcd_panel_io_tx_color(s_io, -1, data, s_line_bytes + s_line_padding);
#if EPD_PROFILE
    s_tx_us += esp_timer_get_time() - w1;
#endif
}

/* Wait for the last in-flight scanline and park CKV/LE. */
static void bus_frame_end(void) {
    while (s_dma_busy) taskYIELD();        /* wait last line (the ISR already latched it) */
    gpio_set_level(s_le_pin, 0);
    gpio_set_level(s_ckv_pin, 1);
}

// MARK: - refresh engine

typedef struct {
    bsp_display_t  base;
    int            width, height;
    uint16_t       line_bytes;
    uint8_t        line_padding;
    uint16_t       gram_stride;   /* bytes per gram row  (width / 2)              */
    uint16_t       dirty_stride;  /* bytes per dirty row (width / 8)              */
    const uint32_t *(*get_waveform_lut)(bsp_epd_mode_t mode, size_t *steps);

    uint8_t       *gram;       /* 4bpp gray, 2 px/byte -- draw target + confirmed */
    uint8_t       *dirty;      /* 1bpp changed-since-confirmed mask               */
    uint16_t      *row_dirty;  /* per-row count of dirty pixels (all-clean skip)  */
    uint8_t       *line[2];    /* DMA scanline ping-pong, internal                */
    uint8_t       *hold_line;  /* constant all-hold (0x00) DMA line               */
    uint8_t        act16[EPD_GRAY_LEVELS];  /* per-frame gray->action table       */

    bsp_epd_mode_t mode;       /* persistent draw mode                            */
    int            oe_pin;
    int            pwr_pin;
} epd_t;

static void power_on(epd_t *s) {
    gpio_set_level(s->oe_pin, 1);  esp_rom_delay_us(100);
    gpio_set_level(s->pwr_pin, 1); esp_rom_delay_us(100);
}

static void power_off(epd_t *s) {
    gpio_set_level(s->pwr_pin, 0); esp_rom_delay_us(1000);
    gpio_set_level(s->oe_pin, 0);  esp_rom_delay_us(100);
}

/* A frame is uniform when every gray column decodes to the same 2-bit drive
 * (code c repeated 16x = c * 0x55555555). Then one prebuilt scanline serves all
 * rows -- the speed win for clears and the solid frames inside a waveform. */
static bool frame_uniform(uint32_t frame, uint8_t *line_byte) {
    if (frame == 0x00000000u || frame == 0x55555555u || frame == 0xAAAAAAAAu) {
        *line_byte = (uint8_t)(frame & 0xFF);   /* 0x00 / 0x55 / 0xAA */
        return true;
    }
    return false;
}

/* Partial refresh: replay `lut` over the whole panel, driving only dirty pixels
 * (the diff-skip path). Clean rows reuse the constant hold line; clean 8-px
 * groups inside a dirty row skip their gram reads. Commits by clearing dirty. */
static void drive_dirty(epd_t *s, const uint32_t *lut, size_t steps) {
    const int W = s->width, H = s->height;
    const int gstride = s->gram_stride, dstride = s->dirty_stride;

#if EPD_PROFILE
    int64_t t_start = esp_timer_get_time();
    int64_t blit_us = 0; int rows_blit = 0;
    s_wait_us = 0; s_tx_us = 0;
#endif
    power_on(s);
    for (size_t f = 0; f < steps; f++) {
        epd_build_act16(lut[f], s->act16);
        bus_frame_begin();
        for (int y = 0; y < H; y++) {
            uint8_t *buf;
            if (s->row_dirty[y] == 0) {
                buf = s->hold_line;
            } else {
                buf = s->line[y & 1];
#if EPD_PROFILE
                int64_t a = esp_timer_get_time();
#endif
                epd_blit_line(W, s->gram + (size_t)y * gstride,
                              s->dirty + (size_t)y * dstride, s->act16, buf);
#if EPD_PROFILE
                blit_us += esp_timer_get_time() - a; rows_blit++;
#endif
            }
            bus_write_line(buf);
        }
        bus_frame_end();
    }
    power_off(s);
#if EPD_PROFILE
    int64_t lines = (int64_t)steps * H;
    ESP_LOGI(TAG, "partial refresh: %dx%d, %u steps, %.2f ms | blit=%.1f us/line (%d blits) "
             "wait=%.1f tx=%.1f us/line",
             W, H, (unsigned)steps, (esp_timer_get_time() - t_start) / 1000.0,
             rows_blit ? (double)blit_us / rows_blit : 0.0, rows_blit,
             (double)s_wait_us / lines, (double)s_tx_us / lines);
#endif

    memset(s->dirty, 0, (size_t)dstride * H);
    memset(s->row_dirty, 0, (size_t)H * sizeof(uint16_t));
}

/* FULL / CLEAR: drive the whole panel by gray (ignoring the dirty mask), a
 * quiescent reset point. Self-contained (owns the power sequence) so it also
 * serves the synchronous bring-up white before any draw. CLEAR additionally
 * forces every pixel's confirmed gray to white. */
static void do_full_drive(epd_t *s, bsp_epd_mode_t mode) {
    bool clear = (mode & ~BSP_EPD_MODE_FULL) == BSP_EPD_MODE_CLEAR;
    size_t steps = 0;
    const uint32_t *lut = s->get_waveform_lut(mode & ~BSP_EPD_MODE_FULL, &steps);
    if (!lut || !steps) return;

    const int W = s->width, H = s->height;
    const int gstride = s->gram_stride;
#if EPD_PROFILE
    int64_t t_start = esp_timer_get_time();
#endif
    power_on(s);
    for (size_t f = 0; f < steps; f++) {
        uint32_t frame = lut[f];
        uint8_t uni;
        bool uniform = frame_uniform(frame, &uni);
        if (uniform) memset(s->line[0], uni, s->line_bytes);
        else epd_build_act16(frame, s->act16);
        bus_frame_begin();
        for (int y = 0; y < H; y++) {
            uint8_t *buf;
            if (uniform) buf = s->line[0];
            else {
                buf = s->line[y & 1];
                epd_blit_line_full(W, s->gram + (size_t)y * gstride, s->act16, buf);
            }
            bus_write_line(buf);
        }
        bus_frame_end();
    }
    power_off(s);
#if EPD_PROFILE
    ESP_LOGI(TAG, "full %s: %dx%d, %u steps, %.2f ms",
             clear ? "clear" : "drive", W, H, (unsigned)steps,
             (esp_timer_get_time() - t_start) / 1000.0);
#endif

    /* Commit: every pixel now shows its target. For CLEAR, white is the confirmed
     * value (nibble 15 in both halves of each byte). Either way, nothing dirty. */
    if (clear) memset(s->gram, 0xFF, (size_t)gstride * H);
    memset(s->dirty, 0, (size_t)s->dirty_stride * H);
    memset(s->row_dirty, 0, (size_t)H * sizeof(uint16_t));
}

// MARK: - vtable

/* Stamp the drawn rect into gram, doing the diff-skip per pixel: where the target
 * gray differs from the current nibble, write it and mark the pixel dirty. Mirrors
 * bsp_blit_rotated's source<->panel mapping. */
static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    epd_t *s = (epd_t *)self;
    const uint8_t *src = pixels;
    const int W = s->width;
    const int gstride = s->gram_stride, dstride = s->dirty_stride;
    const int x0 = area.origin.x, y0 = area.origin.y;
    const int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > W || y0 + h > s->height) {
        return ESP_ERR_INVALID_ARG;
    }

    for (int dr = 0; dr < h; dr++) {
        const int py = y0 + dr;
        uint8_t *grow = s->gram + (size_t)py * gstride;
        uint8_t *drow = s->dirty + (size_t)py * dstride;
        for (int dc = 0; dc < w; dc++) {
            size_t si;
            switch (rotation) {
                case BSP_ROTATION_90:  si = (size_t)dc * h + (h - 1 - dr); break;
                case BSP_ROTATION_270: si = (size_t)(w - 1 - dc) * h + dr; break;
                case BSP_ROTATION_180: si = (size_t)(h - 1 - dr) * w + (w - 1 - dc); break;
                default:               si = (size_t)dr * w + dc; break;
            }
            const int px = x0 + dc;
            const int gray = src[si] >> 4;
            uint8_t *gb = &grow[px >> 1];
            int cur = (px & 1) ? (*gb & 0x0F) : (*gb >> 4);   /* even px = high nibble */
            if (cur == gray) continue;                         /* diff-skip */
            *gb = (px & 1) ? (uint8_t)((*gb & 0xF0) | gray)
                           : (uint8_t)((gray << 4) | (*gb & 0x0F));
            uint8_t m = (uint8_t)(1u << (px & 7));
            uint8_t *db = &drow[px >> 3];
            if (!(*db & m)) { *db |= m; s->row_dirty[py]++; }
        }
    }

    if (s->mode != BSP_EPD_MODE_NONE) {
        return self->refresh(self, area, s->mode);
    }
    return ESP_OK;
}

static esp_err_t op_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    ((epd_t *)self)->mode = mode;
    return ESP_OK;
}

/* Drive a refresh synchronously and return once the panel has settled. A FULL/CLEAR
 * refresh drives the whole panel by gray; a partial refresh drives only the dirty
 * pixels. The gate scan always covers the whole panel, so `area` is unused. */
static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)area;
    epd_t *s = (epd_t *)self;
    if ((mode & ~BSP_EPD_MODE_FULL) == BSP_EPD_MODE_NONE) return ESP_OK;   /* no waveform */

    if (mode & BSP_EPD_MODE_FULL) {
        do_full_drive(s, mode);
        return ESP_OK;
    }
    size_t steps = 0;
    const uint32_t *lut = s->get_waveform_lut(mode, &steps);
    if (lut && steps) drive_dirty(s, lut, steps);
    return ESP_OK;
}

static esp_err_t op_deinit(bsp_display_t *self) {
    epd_t *s = (epd_t *)self;
    power_off(s);
    free(s->gram);
    free(s->dirty);
    free(s->row_dirty);
    free(s->line[0]);
    free(s->line[1]);
    free(s->hold_line);
    free(s);
    return ESP_OK;
}

// MARK: - create

/* Allocate the i80 bus (and so its completion ISR) on a core OTHER than the one that
 * drives refresh. esp_lcd's tx_color ends by re-enabling the trans-done interrupt; with
 * the event pending the ISR runs *synchronously inside tx_color* when called on the
 * ISR's own core -- ~50µs/scanline of in-line ISR work. Refresh is synchronous on the
 * UI/LVGL core, so co-locating the ISR there stalls every line; on a different core the
 * ISR overlaps the next blit instead. esp_lcd registers the ISR on whatever core calls
 * it, so a short-lived task does bus_init pinned to `cfg->task_affinity` (the EPD bus
 * core; must differ from the UI core), defaulting to the core opposite this one. */
static esp_err_t              s_bus_init_err;
static const epd_ll_config_t *s_bus_init_cfg;
static TaskHandle_t           s_bus_init_waiter;

static void bus_init_task(void *arg) {
    (void)arg;
    s_bus_init_err = bus_init(s_bus_init_cfg);
    xTaskNotifyGive(s_bus_init_waiter);
    vTaskDelete(NULL);
}

static esp_err_t bus_init_pinned(const epd_ll_config_t *cfg) {
    int core = cfg->task_affinity;
    if (core < 0 || core >= portNUM_PROCESSORS) {
        core = (xPortGetCoreID() + 1) % portNUM_PROCESSORS;   /* opposite the caller */
    }
    s_bus_init_cfg    = cfg;
    s_bus_init_waiter = xTaskGetCurrentTaskHandle();
    if (xTaskCreatePinnedToCore(bus_init_task, "epd_businit", 4096, NULL, 5, NULL, core) != pdPASS) {
        return bus_init(cfg);   /* fallback: init (and ISR) on this core */
    }
    ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
    return s_bus_init_err;
}

esp_err_t epd_ll_create(const epd_ll_config_t *cfg, bsp_display_t **out_display) {
    epd_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;

    s->base.type         = BSP_DISPLAY_TYPE_DIRECT_EPD;
    s->base.size         = (bsp_size_t){ cfg->width, cfg->height };
    s->base.format       = BSP_PIXEL_FORMAT_L8;
    s->base.draw_bitmap  = op_draw_bitmap;
    s->base.deinit       = op_deinit;
    s->base.set_epd_mode = op_set_epd_mode;
    s->base.refresh      = op_refresh;
    s->mode              = BSP_EPD_MODE_NONE;
    s->oe_pin            = cfg->oe_pin;
    s->pwr_pin           = cfg->pwr_pin;
    s->width             = cfg->width;
    s->height            = cfg->height;
    s->line_bytes        = cfg->line_bytes;
    s->line_padding      = cfg->line_padding;
    s->gram_stride       = (uint16_t)(cfg->width / 2);
    s->dirty_stride      = (uint16_t)(cfg->width / 8);
    s->get_waveform_lut  = cfg->get_waveform_lut;

    size_t gbytes = (size_t)s->gram_stride * cfg->height;
    size_t dbytes = (size_t)s->dirty_stride * cfg->height;
    s->gram      = heap_caps_malloc(gbytes, MALLOC_CAP_SPIRAM);
    s->dirty     = heap_caps_calloc(dbytes, 1, MALLOC_CAP_INTERNAL);
    s->row_dirty = heap_caps_calloc((size_t)cfg->height, sizeof(uint16_t), MALLOC_CAP_INTERNAL);
    if (!s->gram || !s->dirty || !s->row_dirty) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->gram, 0xFF, gbytes);   /* white -- matches the bring-up clear */

    size_t lb = (cfg->line_bytes + cfg->line_padding + 15) & ~(size_t)15;
    for (int i = 0; i < 2; i++) {
        s->line[i] = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
        if (!s->line[i]) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
        memset(s->line[i], 0, lb);   /* padding region stays zero */
    }
    s->hold_line = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
    if (!s->hold_line) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->hold_line, 0, lb);   /* all pixels hold (action 0), padding zero */

    ESP_ERROR_CHECK(bus_init_pinned(cfg));

    /* Power rails off. Configure AFTER bus_init: PWR is lent as the i80 dummy D/C
     * and bus_init hands it back as an output without setting a level, so we own
     * the level here (and the bus init can't clobber it). */
    gpio_config_t gc = {
        .pin_bit_mask = (1ULL << cfg->oe_pin) | (1ULL << cfg->pwr_pin),
        .mode         = GPIO_MODE_OUTPUT,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    ESP_ERROR_CHECK(gpio_config(&gc));
    gpio_set_level(cfg->oe_pin, 0);
    gpio_set_level(cfg->pwr_pin, 0);

    /* Establish a known white screen so later refreshes start from a clean state,
     * driving every pixel to white via the (uniform) clear waveform. */
    do_full_drive(s, BSP_EPD_MODE_CLEAR_FULL);

    *out_display = &s->base;
    return ESP_OK;
}
