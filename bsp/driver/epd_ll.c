/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Direct-drive (TCON-less) grayscale EPD driver for ESP32-S3 -- see epd_ll.h.
 *
 * Two concerns live here, both SoC/bus-specific (re-tailored per peripheral, not
 * abstracted): the low-level i80 bus + CKV/SPV/LE scan protocol (file-static, the
 * `bus_*` helpers), and the refresh engine on top of it. The engine's algorithmic
 * core -- the transaction-index waveform logic -- is the panel-agnostic, SoC-free
 * epd_waveform.h (host unit-tested via driver/test/run.sh); this file adds the
 * framebuffer, the i80 scan, the locking, and the async task around it.
 *
 * Transaction model. There is ONE framebuffer, `state`, a byte per pixel:
 * [7:4] = target/confirmed gray, [3:0] = transaction id. id 0 means idle (the
 * pixel holds; its nibble is the confirmed on-glass value). ids 1..15 are refresh
 * generations that progress independently -- up to 15 in flight -- so disjoint
 * regions can run different waveforms in different phases concurrently.
 *
 *   draw_bitmap  stamps the open generation `pending_id` onto changed pixels only
 *                (the diff-skip is done here, against the byte's current nibble --
 *                no second "displayed" buffer needed) and returns.
 *   refresh      binds a waveform LUT to the open generation, activating it, and
 *                returns (O(1)); the background task drives it.
 *
 * The task runs a continuous frame loop while any generation is ACTIVE: each
 * frame it builds a 256-entry action table from the live generation states,
 * packs every scanline through it (read-only -- the scan never writes `state`, so
 * it cannot race a concurrent draw), and on a generation's final frame reclaims
 * its id->0 in a short locked pass. A FULL/CLEAR refresh aborts every in-flight
 * generation and drives the whole panel uniformly (ghost clear / bring-up white).
 *
 * Locking. A single mutex guards every WRITE to `state`/`tx`/`pending_id` (the
 * draw stamp, the LUT bind, the terminal reclaim) plus the short per-frame table
 * build -- never the multi-row scan. The scan reads `state` unlocked; byte reads
 * are atomic, so a concurrent draw only makes a freshly-drawn pixel render a frame
 * late, never corrupts a byte. (Cost of the single buffer: no frozen snapshot, so
 * redrawing a pixel mid-flight interrupts its waveform -- recovered by the next
 * refresh. The diff-skip and the 15-way pipeline are kept.)
 */

#include "epd_ll.h"
#include "epd_waveform.h"
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "freertos/semphr.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_io_i80.h"
#include "driver/gpio.h"
#include "esp_heap_caps.h"
#include "esp_rom_sys.h"
#include "esp_attr.h"
#include "esp_check.h"

/* Diagnostic: per-frame blit/bus timing split (scan / blit / dma-wait / tx_color).
 * Off by default; set to 1 to re-enable while tuning the refresh path. */
#define EPD_PROFILE 0
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
    const uint32_t *(*get_waveform_lut)(bsp_epd_mode_t mode, size_t *steps);

    uint8_t       *state;      /* [7:4]=gray, [3:0]=tx id -- the only framebuffer */
    uint16_t      *row_nonidle;/* per-row count of non-idle (id != 0) pixels      */
    uint8_t       *line[2];    /* DMA scanline ping-pong, internal                */
    uint8_t       *hold_line;  /* constant all-hold (0x00) DMA line               */
    uint8_t        act_tab[256];/* per-frame action table (task-owned)            */

    epd_tx_t       tx[EPD_TX_SLOTS];
    int            pending_id;  /* open draw generation, 0 = none                 */

    bsp_epd_mode_t mode;        /* persistent draw mode                           */
    int            oe_pin;
    int            pwr_pin;

    TaskHandle_t      task;     /* background waveform engine                     */
    SemaphoreHandle_t mtx;      /* guards state/tx/pending_id writes + table build*/
    SemaphoreHandle_t done;     /* task signals exit on deinit                    */
    SemaphoreHandle_t slot_free;/* binary: posted when the engine frees a slot    */
    bsp_epd_mode_t    full_mode;     /* coalesced pending FULL/CLEAR mode         */
    volatile bool     full_pending;  /* a FULL/CLEAR refresh is queued (aborts all)*/
    volatile bool     running;  /* task is awake (driving), not sleeping on notify*/
    volatile bool     stop;     /* deinit requested                               */
} epd_t;

static void power_on(epd_t *s) {
    gpio_set_level(s->oe_pin, 1);  esp_rom_delay_us(100);
    gpio_set_level(s->pwr_pin, 1); esp_rom_delay_us(100);
}

static void power_off(epd_t *s) {
    gpio_set_level(s->pwr_pin, 0); esp_rom_delay_us(1000);
    gpio_set_level(s->oe_pin, 0);  esp_rom_delay_us(100);
}

static bool tx_any_active(const epd_tx_t tx[EPD_TX_SLOTS]) {
    for (int id = 1; id < EPD_TX_SLOTS; id++) {
        if (tx[id].state == EPD_TX_ACTIVE) return true;
    }
    return false;
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

/* Render one frame of every ACTIVE generation. The table build + the post-scan
 * terminal reclaim are short locked ops; the multi-row scan in between is
 * lock-free and read-only on `state`. */
static void run_one_frame(epd_t *s) {
    const int W = s->width, H = s->height;
    uint16_t active_mask, ended;

    xSemaphoreTake(s->mtx, portMAX_DELAY);
    bool active = epd_frame_mark(s->tx, &active_mask, &ended);
    if (!active) { xSemaphoreGive(s->mtx); return; }
    epd_build_act_tab(s->tx, s->act_tab);
    xSemaphoreGive(s->mtx);

#if EPD_PROFILE
    int64_t t_scan0 = esp_timer_get_time();
    int64_t blit_us = 0;
    int rows_blit = 0;
    s_wait_us = 0; s_tx_us = 0;
#endif
    bus_frame_begin();
    for (int y = 0; y < H; y++) {
        uint8_t *buf;
        if (s->row_nonidle[y] == 0) {
            buf = s->hold_line;                          /* whole row idle: hold */
        } else {
            buf = s->line[y & 1];
#if EPD_PROFILE
            int64_t a = esp_timer_get_time();
#endif
            epd_blit_line(W, s->state + (size_t)y * W, s->act_tab, buf);
#if EPD_PROFILE
            blit_us += esp_timer_get_time() - a; rows_blit++;
#endif
        }
        bus_write_line(buf);
    }
    bus_frame_end();
#if EPD_PROFILE
    int64_t scan_us = esp_timer_get_time() - t_scan0;
    ESP_LOGI(TAG, "QUAL frame: scan=%.1f us/line | blit=%.1f wait=%.1f tx=%.1f (%d/%d blitted)",
             (double)scan_us / H,
             rows_blit ? (double)blit_us / rows_blit : 0.0,
             (double)s_wait_us / H, (double)s_tx_us / H, rows_blit, H);
#endif

    xSemaphoreTake(s->mtx, portMAX_DELAY);
    if (ended) {
        for (int y = 0; y < H; y++) {
            if (!s->row_nonidle[y]) continue;
            int freed = epd_reclaim_row(W, s->state + (size_t)y * W, ended);
            s->row_nonidle[y] -= (uint16_t)freed;
        }
    }
    epd_frame_advance(s->tx, active_mask);
    xSemaphoreGive(s->mtx);
    if (ended) xSemaphoreGive(s->slot_free);   /* a slot freed: wake a blocked draw */
}

/* FULL / CLEAR: abort every in-flight generation and drive the whole panel
 * uniformly. Self-contained (owns the power sequence) so it also serves the
 * synchronous bring-up white before the task exists. Concurrent draws are
 * subsumed -- this is a quiescent reset point, not for use mid-animation. */
static void do_full_drive(epd_t *s, bsp_epd_mode_t mode) {
    bool clear = (mode & ~BSP_EPD_MODE_FULL) == BSP_EPD_MODE_CLEAR;
    size_t steps = 0;
    const uint32_t *lut = s->get_waveform_lut(mode & ~BSP_EPD_MODE_FULL, &steps);
    if (!lut || !steps) return;

    if (s->mtx) xSemaphoreTake(s->mtx, portMAX_DELAY);
    for (int id = 1; id < EPD_TX_SLOTS; id++) s->tx[id].state = EPD_TX_FREE;
    s->pending_id = 0;
    if (s->mtx) xSemaphoreGive(s->mtx);

    const int W = s->width, H = s->height;
    power_on(s);
    for (size_t f = 0; f < steps; f++) {
        uint32_t frame = lut[f];
        uint8_t uni;
        bool uniform = frame_uniform(frame, &uni);
        if (uniform) memset(s->line[0], uni, s->line_bytes);
        else epd_build_full_act_tab(frame, s->act_tab);   /* gray->action, ids ignored */
#if EPD_PROFILE
        s_wait_us = 0; s_tx_us = 0;
        int64_t blit_us = 0;
#endif
        bus_frame_begin();
        for (int y = 0; y < H; y++) {
            uint8_t *buf;
            if (uniform) buf = s->line[0];
            else {
                buf = s->line[y & 1];
#if EPD_PROFILE
                int64_t a = esp_timer_get_time();
#endif
                epd_blit_line(W, s->state + (size_t)y * W, s->act_tab, buf);
#if EPD_PROFILE
                blit_us += esp_timer_get_time() - a;
#endif
            }
            bus_write_line(buf);
        }
        bus_frame_end();
#if EPD_PROFILE
        ESP_LOGI(TAG, "FULL frame %d: uniform=%d | blit=%.1f wait=%.1f tx=%.1f us/line",
                 (int)f, uniform, uniform ? 0.0 : (double)blit_us / H,
                 (double)s_wait_us / H, (double)s_tx_us / H);
#endif
    }
    power_off(s);

    /* Commit: every pixel now shows its target. Clear ids (idle/confirmed); for
     * CLEAR also set the confirmed gray to white. */
    size_t fb = (size_t)W * H;
    if (s->mtx) xSemaphoreTake(s->mtx, portMAX_DELAY);
    if (clear) memset(s->state, 0xF0, fb);
    else for (size_t p = 0; p < fb; p++) s->state[p] &= 0xF0;
    memset(s->row_nonidle, 0, (size_t)H * sizeof(uint16_t));
    if (s->mtx) xSemaphoreGive(s->mtx);
    if (s->slot_free) xSemaphoreGive(s->slot_free);
}

/* Background waveform engine. Sleeps on a task notification when there is nothing
 * to drive; once woken, runs frames continuously -- a queued FULL/CLEAR aborts
 * and drives uniformly, otherwise one frame of the active generations -- powering
 * the panel for the duration and parking it when the queue and all generations
 * drain. */
static void refresh_task(void *arg) {
    epd_t *s = arg;
    bool powered = false;
    for (;;) {
        xSemaphoreTake(s->mtx, portMAX_DELAY);
        if (s->stop) { xSemaphoreGive(s->mtx); break; }
        bool full = s->full_pending;
        bsp_epd_mode_t fmode = s->full_mode;
        bool active = tx_any_active(s->tx);
        if (full) s->full_pending = false;
        if (!full && !active) {
            s->running = false;                  /* idle: re-kick needed to wake us */
            xSemaphoreGive(s->mtx);
            if (powered) { power_off(s); powered = false; }
            ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
            continue;
        }
        xSemaphoreGive(s->mtx);

        if (full) {
            do_full_drive(s, fmode);             /* owns its own power sequence */
            powered = false;
        } else {
            if (!powered) { power_on(s); powered = true; }
            run_one_frame(s);
        }
    }
    if (powered) power_off(s);
    xSemaphoreGive(s->done);
    vTaskDelete(NULL);
}

// MARK: - vtable

/* Stamp the open generation onto the drawn rect, doing the diff-skip per pixel.
 * Mirrors bsp_blit_rotated's source<->panel mapping, but folds compare+stamp into
 * the walk instead of a plain copy (so it can't reuse that helper). */
static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    epd_t *s = (epd_t *)self;
    const uint8_t *src = pixels;
    const int W = s->width;
    const int x0 = area.origin.x, y0 = area.origin.y;
    const int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > W || y0 + h > s->height) {
        return ESP_ERR_INVALID_ARG;
    }

    xSemaphoreTake(s->mtx, portMAX_DELAY);
    while (s->pending_id == 0) {                  /* open a generation for this draw */
        int id = epd_tx_alloc(s->tx);
        if (id) { s->pending_id = id; break; }
        xSemaphoreGive(s->mtx);                   /* slots exhausted: wait for a free */
        xSemaphoreTake(s->slot_free, portMAX_DELAY);
        xSemaphoreTake(s->mtx, portMAX_DELAY);
    }
    const int id = s->pending_id;
    for (int dr = 0; dr < h; dr++) {
        const int py = y0 + dr;
        uint8_t *drow = s->state + (size_t)py * W;
        uint16_t gained = 0;
        for (int dc = 0; dc < w; dc++) {
            size_t si;
            switch (rotation) {
                case BSP_ROTATION_90:  si = (size_t)dc * h + (h - 1 - dr); break;
                case BSP_ROTATION_270: si = (size_t)(w - 1 - dc) * h + dr; break;
                case BSP_ROTATION_180: si = (size_t)(h - 1 - dr) * w + (w - 1 - dc); break;
                default:               si = (size_t)dr * w + dc; break;
            }
            bool now_nonidle;
            uint8_t *cell = &drow[x0 + dc];
            *cell = epd_draw_pixel(*cell, src[si] >> 4, id, &now_nonidle);
            if (now_nonidle) gained++;
        }
        s->row_nonidle[py] += gained;
    }
    xSemaphoreGive(s->mtx);

    if (s->mode != BSP_EPD_MODE_NONE) {
        return self->refresh(self, area, s->mode);
    }
    return ESP_OK;
}

static esp_err_t op_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    ((epd_t *)self)->mode = mode;
    return ESP_OK;
}

/* Coalesce two pending FULL/CLEAR modes: keep the stronger waveform
 * (CLEAR > QUALITY > FAST by enum value) and the FULL flag. */
static bsp_epd_mode_t full_coalesce(bsp_epd_mode_t a, bsp_epd_mode_t b) {
    bsp_epd_mode_t wa = a & ~BSP_EPD_MODE_FULL, wb = b & ~BSP_EPD_MODE_FULL;
    return (wa > wb ? wa : wb) | BSP_EPD_MODE_FULL;
}

/* Enqueue a refresh and return immediately; the background task does the scan.
 * A partial refresh binds the open generation's waveform and activates it (O(1));
 * a FULL/CLEAR refresh is queued to abort everything and drive uniformly. The
 * gate scan always covers the whole panel, so `area` is unused (pixels carry
 * their own generation id). */
static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)area;
    epd_t *s = (epd_t *)self;
    if ((mode & ~BSP_EPD_MODE_FULL) == BSP_EPD_MODE_NONE) return ESP_OK;   /* no waveform */

    bool wake = false;
    xSemaphoreTake(s->mtx, portMAX_DELAY);
    if (mode & BSP_EPD_MODE_FULL) {
        s->full_mode = s->full_pending ? full_coalesce(s->full_mode, mode) : mode;
        s->full_pending = true;
        if (!s->running) { s->running = true; wake = true; }
    } else if (s->pending_id) {
        size_t steps = 0;
        const uint32_t *lut = s->get_waveform_lut(mode & ~BSP_EPD_MODE_FULL, &steps);
        if (lut && steps) {                       /* unknown mode (no LUT): leave open */
            int id = s->pending_id;
            s->tx[id].lut   = lut;
            s->tx[id].steps = (uint16_t)steps;
            s->tx[id].frame = 0;
            s->tx[id].state = EPD_TX_ACTIVE;
            s->pending_id   = 0;                  /* close the generation */
            if (!s->running) { s->running = true; wake = true; }
        }
    }
    xSemaphoreGive(s->mtx);
    if (wake) xTaskNotifyGive(s->task);
    return ESP_OK;
}

static esp_err_t op_deinit(bsp_display_t *self) {
    epd_t *s = (epd_t *)self;
    if (s->task) {                       /* stop and join the background task */
        xSemaphoreTake(s->mtx, portMAX_DELAY);
        s->stop = true;
        xSemaphoreGive(s->mtx);
        xTaskNotifyGive(s->task);
        xSemaphoreTake(s->done, portMAX_DELAY);
    }
    if (s->mtx)       vSemaphoreDelete(s->mtx);
    if (s->done)      vSemaphoreDelete(s->done);
    if (s->slot_free) vSemaphoreDelete(s->slot_free);
    power_off(s);
    free(s->state);
    free(s->row_nonidle);
    free(s->line[0]);
    free(s->line[1]);
    free(s->hold_line);
    free(s);
    return ESP_OK;
}

// MARK: - create

/* esp_lcd installs the i80 trans-done ISR on whichever core calls bus_init, and
 * re-enables it inline in tx_color -- so a same-core ISR runs synchronously inside the
 * scan's tx_color (~50µs/line stall). Init from a short-lived task pinned OPPOSITE the
 * refresh task so the ISR overlaps the next blit instead of stalling it. */
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
    int core;
    if (cfg->task_affinity < 0 || cfg->task_affinity >= portNUM_PROCESSORS) {
        core = (xPortGetCoreID() + 1) % portNUM_PROCESSORS;        /* opposite the caller */
    } else {
        core = (cfg->task_affinity + 1) % portNUM_PROCESSORS;      /* opposite the refresh task */
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
    s->get_waveform_lut  = cfg->get_waveform_lut;

    size_t fb = (size_t)cfg->width * cfg->height;
    s->state       = heap_caps_malloc(fb, MALLOC_CAP_SPIRAM);
    s->row_nonidle = heap_caps_calloc((size_t)cfg->height, sizeof(uint16_t), MALLOC_CAP_SPIRAM);
    if (!s->state || !s->row_nonidle) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->state, 0xF0, fb);   /* white, idle (id 0) -- matches the bring-up clear */

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

    s->mtx       = xSemaphoreCreateMutex();
    s->done      = xSemaphoreCreateBinary();
    s->slot_free = xSemaphoreCreateBinary();
    if (!s->mtx || !s->done || !s->slot_free) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }

    /* Establish a known white screen so later refreshes start from a clean state.
     * Synchronous -- runs before the task exists, driving every pixel to white via
     * the (uniform) clear waveform. */
    do_full_drive(s, BSP_EPD_MODE_CLEAR_FULL);

    /* Bring up the async waveform engine. Caller picks the priority/core
     * (default core 0 keeps its DMA busy-waits off the UI/LVGL core). */
    BaseType_t core = cfg->task_affinity < 0 ? tskNO_AFFINITY : cfg->task_affinity;
    if (xTaskCreatePinnedToCore(refresh_task, "epd_refresh", 4096, s, cfg->task_priority, &s->task, core) != pdPASS) {
        s->task = NULL;
        op_deinit(&s->base);
        return ESP_ERR_NO_MEM;
    }

    *out_display = &s->base;
    return ESP_OK;
}
