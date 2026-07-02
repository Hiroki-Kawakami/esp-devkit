/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Direct-drive (TCON-less) grayscale EPD driver for ESP32-S3 -- see epd_ll.h.
 *
 * Two concerns live here, both SoC/bus-specific (re-tailored per peripheral, not
 * abstracted): the low-level i80 bus + CKV/SPV/LE scan protocol (file-static, the
 * `bus_*` helpers), and the refresh engine on top of it. The engine's algorithmic
 * core -- the per-pixel waveform logic -- is the panel-agnostic, SoC-free
 * epd_waveform.h (host unit-tested via driver/epd/test/run.sh); this file adds
 * the framebuffer, the i80 scan, the locking, and the async task around it.
 *
 * Model. One framebuffer, `state`, a uint16 per pixel carrying the whole
 * life-cycle: confirmed gray, target gray, waveform id, and the start frame of
 * its replay (encoding in epd_waveform.h). Every pixel progresses its own
 * waveform independently -- there is no generation/slot limit, and refresh(area)
 * really is area-scoped:
 *
 *   draw_bitmap  diff-skips against each pixel's target and stamps changed
 *                pixels PENDING. A pixel mid-waveform toward a DIFFERENT gray is
 *                a conflict: the call BLOCKS until that pixel retires (waveforms
 *                are never interrupted -- interrupting unbalances the panel's DC
 *                offset). While anyone is blocked the engine skips waveforms'
 *                skippable settle tails (DC-neutral) to release them sooner.
 *   refresh      arms the PENDING pixels inside `area` with the mode's waveform
 *                and returns; the background task drives them. With
 *                BSP_EPD_MODE_ALL it first waits for every in-flight pixel in
 *                the area to retire, then arms ALL pixels (idle ones re-drive
 *                to their confirmed gray -- the from==to LUT diagonal is the
 *                ghost clear), so the whole area replays uniformly.
 *   clear        (vtable op) waits for every in-flight pixel, then arms the
 *                whole panel to white with the CLEAR waveform. Also run
 *                synchronously at create for the bring-up white baseline. A
 *                draw issued while the clear replays blocks on the conflict --
 *                and the CLEAR settle tail is non-skippable -- so a
 *                clear-then-draw sequence can never cut the clear short.
 *
 * The task runs a continuous frame loop while any pixel is in flight: each frame
 * it bumps the mod-64 frame counter, builds the 256-entry b1 tables, packs every
 * scanline through them (read-only -- the scan never writes `state`, so it
 * cannot race a concurrent draw), and reclaims retiring pixels in a short locked
 * pass. Frames whose only live rowset drives uniformly (the clear waveform)
 * reuse one prebuilt scanline for every fully-active row.
 *
 * Locking. A single mutex guards every WRITE to `state` and the counters (the
 * draw stamp, the arming, the retire pass) plus the short per-frame table
 * build -- never the multi-row scan. The scan reads `state` unlocked; a pixel
 * armed mid-scan starts on the NEXT frame (epd_waveform.h's armed step), so a
 * torn frame cannot skip a waveform's first step. Blocked callers wait on
 * `retire_evt`, which the engine posts every frame while `waiters` > 0.
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

/* Byte-table pool depth = max distinct driving (lut, step) pairs per frame;
 * beyond it the frame falls back to the word/shift blit (epd_blit_line). */
#define EPD_ACT_POOL 16

typedef struct {
    bsp_display_t  base;
    int            width, height;
    uint16_t       line_bytes;
    uint8_t        line_padding;

    epd_waveform_t wf[EPD_WF_SLOTS];  /* bound once at create; CLEAR mandatory */

    uint16_t      *state;       /* one word per pixel -- see epd_waveform.h    */
    uint16_t      *row_nonidle; /* per-row count of non-IDLE pixels            */
    uint8_t       *line[2];     /* DMA scanline ping-pong, internal            */
    uint8_t       *hold_line;   /* constant all-hold (0x00) DMA line           */
    uint8_t       *uni_line;    /* prebuilt line for uniform frames            */

    const uint32_t *rowset_tab[256];  /* per-frame b1 tables (engine-owned)    */
    uint8_t         retire_tab[256];
    /* Per-frame flat byte tables for the hot blit (epd_build_act256): one pool
     * slot per live driving b1, everything else the shared all-hold table. All
     * internal RAM, so the scan never touches the flash-resident LUTs. */
    const uint8_t  *act_tabs[256];
    uint8_t         act_pool[EPD_ACT_POOL][256];
    uint8_t         act_hold[256];    /* stays zero (calloc)                   */
    uint32_t        group[256]; /* live pixel count per b1 (IDLE untracked)    */
    uint32_t        active_px;  /* pixels currently replaying a waveform       */
    uint8_t         frame;      /* engine frame counter, mod 64                */
    volatile int    waiters;    /* callers blocked on an in-flight region      */

    bsp_epd_mode_t mode;        /* persistent draw mode                        */
    int            oe_pin;
    int            pwr_pin;

    TaskHandle_t      task;     /* background waveform engine                  */
    SemaphoreHandle_t mtx;      /* guards state/counter writes + table build   */
    SemaphoreHandle_t done;     /* task signals exit on deinit                 */
    SemaphoreHandle_t retire_evt; /* posted each retiring frame / while waiters */
    volatile bool     running;  /* task is awake (driving), not sleeping       */
    volatile bool     stop;     /* deinit requested                            */
} epd_t;

static void power_on(epd_t *s) {
    gpio_set_level(s->oe_pin, 1);  esp_rom_delay_us(100);
    gpio_set_level(s->pwr_pin, 1); esp_rom_delay_us(100);
}

static void power_off(epd_t *s) {
    gpio_set_level(s->pwr_pin, 0); esp_rom_delay_us(1000);
    gpio_set_level(s->oe_pin, 0);  esp_rom_delay_us(100);
}

/* Caller holds mtx and has found an in-flight conflict: wait one retire cycle
 * and re-take the lock. The engine posts retire_evt every frame while
 * waiters > 0 (and engages settle-skip for the same reason), so this returns at
 * frame cadence for a re-check. */
static void wait_retire(epd_t *s) {
    s->waiters++;
    xSemaphoreGive(s->mtx);
    xSemaphoreTake(s->retire_evt, portMAX_DELAY);
    xSemaphoreTake(s->mtx, portMAX_DELAY);
    s->waiters--;
}

/* Render one frame of every in-flight pixel, then reclaim the ones that
 * finished. The table build + the retire pass are short locked ops; the
 * multi-row scan in between is lock-free and read-only on `state`. */
static void run_one_frame(epd_t *s) {
    const int W = s->width, H = s->height;

    xSemaphoreTake(s->mtx, portMAX_DELAY);
    s->frame = (uint8_t)((s->frame + 1) & 63);
    epd_build_frame_tab(s->wf, s->frame, s->waiters > 0, s->rowset_tab, s->retire_tab);

    /* Live-group sweep: does anything retire this frame, and do all in-flight
     * pixels share one uniformly-driving rowset (the clear-waveform fast path:
     * one prebuilt scanline serves every fully-active row)? */
    bool any_retire = false, multiple = false;
    const uint32_t *only = NULL;
    for (int b1 = 0; b1 < (EPD_WF_SLOTS << 6); b1++) {
        if (!s->group[b1]) continue;
        if (s->retire_tab[b1]) any_retire = true;
        if (!only) only = s->rowset_tab[b1];
        else if (only != s->rowset_tab[b1]) multiple = true;
    }
    uint8_t uni_byte = 0;
    bool uniform = only && !multiple && s->group[EPD_B1_PENDING] == 0 &&
                   epd_rowset_uniform(only, &uni_byte);
    if (uniform) memset(s->uni_line, uni_byte, s->line_bytes);

    /* Expand each live driving rowset into an internal-RAM byte table. */
    bool tab_ok = true;
    for (int i = 0; i < 256; i++) s->act_tabs[i] = s->act_hold;
    int used = 0;
    for (int b1 = 0; b1 < (EPD_WF_SLOTS << 6); b1++) {
        if (!s->group[b1] || s->rowset_tab[b1] == epd_hold_rowset) continue;
        if (used == EPD_ACT_POOL) { tab_ok = false; break; }
        epd_build_act256(s->rowset_tab[b1], s->act_pool[used]);
        s->act_tabs[b1] = s->act_pool[used++];
    }
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
        } else if (uniform && s->row_nonidle[y] == (uint16_t)W) {
            buf = s->uni_line;
        } else {
            buf = s->line[y & 1];
#if EPD_PROFILE
            int64_t a = esp_timer_get_time();
#endif
            if (tab_ok) epd_blit_line_tab(W, s->state + (size_t)y * W, s->act_tabs, buf);
            else        epd_blit_line(W, s->state + (size_t)y * W, s->rowset_tab, buf);
#if EPD_PROFILE
            blit_us += esp_timer_get_time() - a; rows_blit++;
#endif
        }
        bus_write_line(buf);
    }
    bus_frame_end();
#if EPD_PROFILE
    int64_t scan_us = esp_timer_get_time() - t_scan0;
    ESP_LOGI(TAG, "frame %u: scan=%.1f us/line | blit=%.1f wait=%.1f tx=%.1f (%d/%d blitted, uni=%d)",
             s->frame, (double)scan_us / H,
             rows_blit ? (double)blit_us / rows_blit : 0.0,
             (double)s_wait_us / H, (double)s_tx_us / H, rows_blit, H, uniform);
#endif

    int freed_total = 0;
    if (any_retire) {
        xSemaphoreTake(s->mtx, portMAX_DELAY);
        for (int y = 0; y < H; y++) {
            if (!s->row_nonidle[y]) continue;
            int freed = epd_retire_row(W, s->state + (size_t)y * W, s->retire_tab);
            s->row_nonidle[y] -= (uint16_t)freed;
            freed_total += freed;
        }
        for (int b1 = 0; b1 < (EPD_WF_SLOTS << 6); b1++) {
            if (s->retire_tab[b1]) s->group[b1] = 0;
        }
        s->active_px -= (uint32_t)freed_total;
        xSemaphoreGive(s->mtx);
    }
    if (freed_total || s->waiters > 0) xSemaphoreGive(s->retire_evt);
}

/* Background waveform engine. Sleeps on a task notification when no pixel is in
 * flight; once woken, runs frames continuously, powering the panel for the
 * duration and parking it when everything retires. */
static void refresh_task(void *arg) {
    epd_t *s = arg;
    bool powered = false;
    for (;;) {
        xSemaphoreTake(s->mtx, portMAX_DELAY);
        if (s->stop) { xSemaphoreGive(s->mtx); break; }
        if (s->active_px == 0) {
            s->running = false;                  /* idle: re-kick needed to wake us */
            xSemaphoreGive(s->mtx);
            if (powered) { power_off(s); powered = false; }
            ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
            continue;
        }
        xSemaphoreGive(s->mtx);
        if (!powered) { power_on(s); powered = true; }
        run_one_frame(s);
    }
    if (powered) power_off(s);
    xSemaphoreGive(s->done);
    vTaskDelete(NULL);
}

// MARK: - vtable

/* Stamp the drawn rect PENDING, diff-skipping per pixel. Mirrors
 * bsp_blit_rotated's source<->panel mapping, but folds compare+stamp into the
 * walk instead of a plain copy (so it can't reuse that helper). A conflict (a
 * pixel mid-waveform toward a different gray) blocks and retries the walk --
 * the partial PENDING stamps are idempotent across retries. */
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
    for (;;) {
        bool conflict = false;
        for (int dr = 0; dr < h; dr++) {
            const int py = y0 + dr;
            uint16_t *drow = s->state + (size_t)py * W;
            uint16_t gained = 0;
            for (int dc = 0; dc < w; dc++) {
                size_t si;
                switch (rotation) {
                    case BSP_ROTATION_90:  si = (size_t)dc * h + (h - 1 - dr); break;
                    case BSP_ROTATION_270: si = (size_t)(w - 1 - dc) * h + dr; break;
                    case BSP_ROTATION_180: si = (size_t)(h - 1 - dr) * w + (w - 1 - dc); break;
                    default:               si = (size_t)dr * w + dc; break;
                }
                bool c, now_nonidle;
                uint16_t *cell = &drow[x0 + dc];
                *cell = epd_draw_px(*cell, src[si] >> 4, &c, &now_nonidle);
                conflict |= c;
                if (now_nonidle) gained++;
            }
            s->row_nonidle[py] += gained;
            s->group[EPD_B1_PENDING] += gained;
        }
        if (!conflict) break;
        wait_retire(s);
    }
    xSemaphoreGive(s->mtx);

    if ((s->mode & ~BSP_EPD_MODE_ALL) != BSP_EPD_MODE_NONE) {
        return self->refresh(self, area, s->mode);
    }
    return ESP_OK;
}

static esp_err_t op_set_epd_mode(bsp_display_t *self, bsp_epd_mode_t mode) {
    ((epd_t *)self)->mode = mode;
    return ESP_OK;
}

static int mode_to_wf(bsp_epd_mode_t mode) {
    switch (mode & ~BSP_EPD_MODE_ALL) {
        case BSP_EPD_MODE_FAST:    return EPD_LL_WAVEFORM_FAST;
        case BSP_EPD_MODE_QUALITY: return EPD_LL_WAVEFORM_QUALITY;
        default:                   return -1;
    }
}

/* Is any pixel in the rect still replaying a waveform? (early-out; rows with no
 * non-idle pixels are skipped) */
static bool area_has_active(const epd_t *s, int x0, int y0, int x1, int y1) {
    for (int y = y0; y < y1; y++) {
        if (!s->row_nonidle[y]) continue;
        const uint16_t *row = s->state + (size_t)y * s->width;
        for (int x = x0; x < x1; x++) {
            if (epd_px_is_active(row[x])) return true;
        }
    }
    return false;
}

/* Arm PENDING pixels inside `area` with the mode's waveform and return; the
 * background task drives them. With BSP_EPD_MODE_ALL, first BLOCK until no
 * pixel in the area is in flight, then arm every pixel (idle ones re-drive
 * from==to) so the whole area replays uniformly from step 0. */
static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    epd_t *s = (epd_t *)self;
    int wfid = mode_to_wf(mode);
    if (wfid < 0 || !s->wf[wfid].lut) return ESP_OK;   /* no waveform: leave pending */

    int x0 = area.origin.x, y0 = area.origin.y;
    int x1 = x0 + area.size.width, y1 = y0 + area.size.height;
    if (x0 < 0) x0 = 0;
    if (y0 < 0) y0 = 0;
    if (x1 > s->width)  x1 = s->width;
    if (y1 > s->height) y1 = s->height;
    if (x0 >= x1 || y0 >= y1) return ESP_OK;
    const bool all = (mode & BSP_EPD_MODE_ALL) != 0;

    xSemaphoreTake(s->mtx, portMAX_DELAY);
    if (all) {
        while (area_has_active(s, x0, y0, x1, y1)) wait_retire(s);
    }
    const uint8_t b1 = epd_b1_armed(wfid, s->frame);
    uint32_t armed = 0;
    for (int y = y0; y < y1; y++) {
        if (!all && !s->row_nonidle[y]) continue;
        uint16_t *row = s->state + (size_t)y * s->width;
        uint16_t row_armed = 0, row_new_nonidle = 0;
        for (int x = x0; x < x1; x++) {
            const uint8_t pb1 = (uint8_t)(row[x] >> 8);
            if (pb1 == EPD_B1_PENDING) {
                row[x] = (uint16_t)((row[x] & 0x00FF) | (b1 << 8));
                s->group[EPD_B1_PENDING]--;
                row_armed++;
            } else if (all && pb1 == EPD_B1_IDLE) {
                row[x] = (uint16_t)((row[x] & 0x00FF) | (b1 << 8));
                row_armed++;
                row_new_nonidle++;
            }
        }
        s->row_nonidle[y] += row_new_nonidle;
        armed += row_armed;
    }
    bool wake = false;
    if (armed) {
        s->group[b1] += armed;
        s->active_px += armed;
        if (!s->running) { s->running = true; wake = true; }
    }
    xSemaphoreGive(s->mtx);
    if (wake) xTaskNotifyGive(s->task);
    return ESP_OK;
}

/* Arm the whole panel to white with the CLEAR waveform. Blocks until every
 * in-flight pixel retires first (a clear is a quiescent reset point; the
 * never-interrupt rule applies to it too); PENDING draws are subsumed. Also run
 * synchronously at create (task == NULL) for the bring-up white baseline. */
static esp_err_t op_clear(bsp_display_t *self) {
    epd_t *s = (epd_t *)self;
    const int W = s->width, H = s->height;
    const size_t fb = (size_t)W * H;

    xSemaphoreTake(s->mtx, portMAX_DELAY);
    while (s->active_px) wait_retire(s);
    const uint8_t b1 = epd_b1_armed(EPD_LL_WAVEFORM_CLEAR, s->frame);
    for (size_t p = 0; p < fb; p++) {
        s->state[p] = (uint16_t)((b1 << 8) | (s->state[p] & 0x00F0) | 15);
    }
    for (int y = 0; y < H; y++) s->row_nonidle[y] = (uint16_t)W;
    s->group[EPD_B1_PENDING] = 0;
    s->group[b1]  = (uint32_t)fb;
    s->active_px  = (uint32_t)fb;
    bool wake = s->task && !s->running;
    if (wake) s->running = true;
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
    if (s->mtx)        vSemaphoreDelete(s->mtx);
    if (s->done)       vSemaphoreDelete(s->done);
    if (s->retire_evt) vSemaphoreDelete(s->retire_evt);
    power_off(s);
    free(s->state);
    free(s->row_nonidle);
    free(s->line[0]);
    free(s->line[1]);
    free(s->hold_line);
    free(s->uni_line);
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
    s->base.clear        = op_clear;
    s->mode              = BSP_EPD_MODE_NONE;
    s->oe_pin            = cfg->oe_pin;
    s->pwr_pin           = cfg->pwr_pin;
    s->width             = cfg->width;
    s->height            = cfg->height;
    s->line_bytes        = cfg->line_bytes;
    s->line_padding      = cfg->line_padding;

    /* Bind + validate the panel's waveforms (CLEAR is mandatory: it backs the
     * clear op and the bring-up white baseline). */
    for (int i = 0; i < EPD_WF_SLOTS; i++) {
        size_t steps = 0;
        epd_ll_lut_t lut = cfg->get_waveform_lut((epd_ll_waveform_t)i, &steps);
        if (!lut || !steps) continue;
        if (!epd_waveform_init(&s->wf[i], lut, steps)) { free(s); return ESP_ERR_INVALID_ARG; }
    }
    if (!s->wf[EPD_LL_WAVEFORM_CLEAR].lut) { free(s); return ESP_ERR_INVALID_ARG; }

    size_t fb = (size_t)cfg->width * cfg->height;
    s->state       = heap_caps_malloc(fb * sizeof(uint16_t), MALLOC_CAP_SPIRAM);
    s->row_nonidle = heap_caps_calloc((size_t)cfg->height, sizeof(uint16_t), MALLOC_CAP_SPIRAM);
    if (!s->state || !s->row_nonidle) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->state, 0xFF, fb * sizeof(uint16_t));   /* white, IDLE (0xFFFF) */

    size_t lb = (cfg->line_bytes + cfg->line_padding + 15) & ~(size_t)15;
    for (int i = 0; i < 2; i++) {
        s->line[i] = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
        if (!s->line[i]) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
        memset(s->line[i], 0, lb);   /* padding region stays zero */
    }
    s->hold_line = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
    s->uni_line  = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
    if (!s->hold_line || !s->uni_line) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->hold_line, 0, lb);   /* all pixels hold (action 0), padding zero */
    memset(s->uni_line, 0, lb);

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

    s->mtx        = xSemaphoreCreateMutex();
    s->done       = xSemaphoreCreateBinary();
    s->retire_evt = xSemaphoreCreateBinary();
    if (!s->mtx || !s->done || !s->retire_evt) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }

    /* Establish a known white screen so later refreshes start from a clean
     * state. Synchronous -- the task doesn't exist yet, so pump the frame loop
     * here (the clear waveform's uniform frames take the prebuilt-line path). */
    op_clear(&s->base);
    power_on(s);
    while (s->active_px) run_one_frame(s);
    power_off(s);

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
