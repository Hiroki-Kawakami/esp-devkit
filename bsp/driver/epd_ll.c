/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Direct-drive (TCON-less) grayscale EPD driver for ESP32-S3 -- see epd_ll.h.
 *
 * Two concerns live here, both SoC/bus-specific (re-tailored per peripheral, not
 * abstracted): the low-level i80 bus + CKV/SPV/LE scan protocol (file-static, the
 * `bus_*` helpers), and the refresh engine on top of it (the framebuffer, the
 * time-axis waveform LUT, differential drive, and the async refresh task). The
 * engine is panel-agnostic -- geometry, scanline format, power pins, and the
 * waveform LUTs all arrive via epd_ll_config_t. epd_ll_create wires them together
 * and returns a bsp_display_t.
 *
 * Refresh model. draw_bitmap updates an L8 target GRAM and returns; refresh()
 * coalesces a request and wakes the background task, also returning immediately.
 * The task freezes GRAM into a `snapshot`, then replays the waveform LUT,
 * rebuilding every scanline from the snapshot each frame. Only pixels whose
 * target differs from the displayed state (`disp`) are driven; the rest hold. The
 * BSP_EPD_MODE_FULL flag forces every pixel to drive (ghost clear). Gate scanning
 * always covers the whole panel, so refresh ignores the dirty rect.
 *
 * Coherency: three full-frame buffers. `gram` is written by the caller (UI),
 * `snapshot` is the frozen target the task drives, `disp` is the committed
 * on-glass state. A mutex guards only the short ops -- GRAM writes, the
 * gram->snapshot copy, the busy/pending state -- never the multi-frame scan. Each
 * refresh diffs snapshot vs disp and commits disp = snapshot; draws that land
 * mid-scan are picked up by the next (coalesced) refresh.
 *
 * Scanline packing: 4 source pixels -> one byte, leftmost pixel in the high 2-bit
 * pair (MSB-first), giving width/4 bytes per line. Rows with no driven pixel reuse
 * a constant all-hold line; uniform frames (every gray drives the same) likewise
 * reuse a single prebuilt line for all rows.
 */

#include "epd_ll.h"
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

static const char *TAG = "epd_ll";

// MARK: - i80 bus + CKV/SPV/LE scan protocol (singleton, internal)

static esp_lcd_i80_bus_handle_t   s_i80_bus;
static esp_lcd_panel_io_handle_t  s_io;
static volatile bool              s_dma_busy;   /* a scanline DMA is in flight */
static int      s_ckv_pin, s_spv_pin, s_le_pin;
static uint16_t s_line_bytes;
static uint8_t  s_line_padding;

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
    while (s_dma_busy) taskYIELD();        /* wait previous scanline DMA + its latch */
    s_dma_busy = true;
    gpio_set_level(s_le_pin, 0);           /* close the latch before shifting */
    gpio_set_level(s_ckv_pin, 1);          /* CKV high for this line */
    esp_lcd_panel_io_tx_color(s_io, -1, data, s_line_bytes + s_line_padding);
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

    uint8_t       *gram;       /* L8 target (caller-written), PSRAM   */
    uint8_t       *snapshot;   /* L8 frozen target the task drives    */
    uint8_t       *disp;       /* L8 displayed state, PSRAM           */
    bool          *row_drv;    /* per-row "needs drive", height bytes */
    uint8_t       *line[2];    /* DMA scanline ping-pong, internal    */
    uint8_t       *hold_line;  /* constant all-hold (0x00) DMA line   */
    bsp_epd_mode_t mode;       /* persistent draw mode                */
    int            oe_pin;
    int            pwr_pin;

    TaskHandle_t      task;    /* background waveform engine          */
    SemaphoreHandle_t mtx;     /* guards gram write / snapshot / state */
    SemaphoreHandle_t done;    /* task signals exit on deinit         */
    bsp_epd_mode_t    pending_mode;  /* coalesced next refresh        */
    volatile bool     busy;    /* a refresh is in flight              */
    volatile bool     pending; /* a refresh is queued                 */
    volatile bool     stop;    /* deinit requested                    */
} epd_t;

static void power_on(epd_t *s) {
    gpio_set_level(s->oe_pin, 1);  esp_rom_delay_us(100);
    gpio_set_level(s->pwr_pin, 1); esp_rom_delay_us(100);
}

static void power_off(epd_t *s) {
    gpio_set_level(s->pwr_pin, 0); esp_rom_delay_us(1000);
    gpio_set_level(s->oe_pin, 0);  esp_rom_delay_us(100);
}

/* Build one DMA scanline from a GRAM row (L8) for the given waveform frame.
 * 4 pixels pack into one byte, leftmost pixel in the high 2-bit pair (MSB-first);
 * byte k holds pixels 4k..4k+3 -> width/4 bytes per line. A pixel drives only
 * when its target nibble differs from the displayed one; a NULL disp_row drives
 * every pixel (full refresh). Pixels that hold emit action 0. */
static void build_line(int width, const uint8_t *row, const uint8_t *disp_row,
                       uint32_t frame, uint8_t *dst) {
    for (int x = 0; x < width; x += 4) {
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
static bool row_changed(int width, const uint8_t *row, const uint8_t *disp_row) {
    for (int x = 0; x < width; x++) {
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
 * the uniform fast path. The displayed state is committed to match the target
 * after the scan. */
static void refresh_lut(epd_t *s, const uint8_t *src,
                        const uint32_t *lut, size_t steps, bool full) {
    const int W = s->width, H = s->height;
    bool any = false;
    for (int y = 0; y < H; y++) {
        bool d = full || row_changed(W, src + (size_t)y * W, s->disp + (size_t)y * W);
        s->row_drv[y] = d;
        any |= d;
    }
    if (!any) return;   /* nothing changed: skip power-up and the whole scan */

    power_on(s);
    for (size_t f = 0; f < steps; f++) {
        uint32_t frame = lut[f];
        uint8_t uni_byte;
        bool uniform = full && frame_uniform(frame, &uni_byte);
        if (uniform) memset(s->line[0], uni_byte, s->line_bytes);

        bus_frame_begin();
        for (int y = 0; y < H; y++) {
            uint8_t *buf;
            if (!s->row_drv[y]) {
                buf = s->hold_line;   /* whole row holds */
            } else if (uniform) {
                buf = s->line[0];     /* same constant line for every row */
            } else {
                buf = s->line[y & 1];
                build_line(W, src + (size_t)y * W,
                           full ? NULL : s->disp + (size_t)y * W, frame, buf);
            }
            bus_write_line(buf);
        }
        bus_frame_end();
    }
    power_off(s);

    memcpy(s->disp, src, (size_t)W * H);
}

/* Look up the waveform for a refresh mode (FULL stripped) and replay it. */
static void do_refresh(epd_t *s, const uint8_t *src, bsp_epd_mode_t mode) {
    bool full = mode & BSP_EPD_MODE_FULL;
    size_t steps = 0;
    const uint32_t *lut = s->get_waveform_lut(mode & ~BSP_EPD_MODE_FULL, &steps);
    if (lut && steps) refresh_lut(s, src, lut, steps, full);
}

/* Coalesce two pending refresh modes: keep FULL if either has it, and the
 * stronger waveform (QUALITY > FAST > NONE by enum value). */
static bsp_epd_mode_t mode_coalesce(bsp_epd_mode_t a, bsp_epd_mode_t b) {
    bsp_epd_mode_t full = (a | b) & BSP_EPD_MODE_FULL;
    bsp_epd_mode_t wa = a & ~BSP_EPD_MODE_FULL, wb = b & ~BSP_EPD_MODE_FULL;
    return (wa > wb ? wa : wb) | full;
}

/* Background waveform engine. Sleeps on a task notification; once woken, drains
 * coalesced refreshes -- snapshotting GRAM under the mutex, then driving the scan
 * lock-free -- until none remain, then sleeps again. */
static void refresh_task(void *arg) {
    epd_t *s = arg;
    const size_t fb = (size_t)s->width * s->height;
    for (;;) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
        for (;;) {
            xSemaphoreTake(s->mtx, portMAX_DELAY);
            if (s->stop)      { xSemaphoreGive(s->mtx); goto done; }
            if (!s->pending)  { s->busy = false; xSemaphoreGive(s->mtx); break; }
            bsp_epd_mode_t m = s->pending_mode;
            s->pending = false;
            s->pending_mode = BSP_EPD_MODE_NONE;
            memcpy(s->snapshot, s->gram, fb);   /* freeze target under the lock */
            xSemaphoreGive(s->mtx);
            do_refresh(s, s->snapshot, m);
        }
    }
done:
    xSemaphoreGive(s->done);
    vTaskDelete(NULL);
}

// MARK: - vtable

static esp_err_t op_draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                                bsp_rotation_t rotation) {
    epd_t *s = (epd_t *)self;
    const uint8_t *src = pixels;
    int x0 = area.origin.x, y0 = area.origin.y;
    int w = area.size.width, h = area.size.height;
    if (x0 < 0 || y0 < 0 || x0 + w > s->width || y0 + h > s->height) {
        return ESP_ERR_INVALID_ARG;
    }
    /* Guard GRAM against the task's snapshot copy (the lock is held only for the
     * copy, never the scan). */
    xSemaphoreTake(s->mtx, portMAX_DELAY);
    if (rotation == BSP_ROTATION_0) {
        for (int r = 0; r < h; r++) {
            memcpy(s->gram + (size_t)(y0 + r) * s->width + x0, src + (size_t)r * w, w);
        }
    } else {
        bsp_blit_rotated(s->gram, s->width, 1, area, pixels, rotation);
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

/* Enqueue a refresh and return immediately; the background task does the scan.
 * Concurrent requests coalesce (stronger waveform / FULL wins) into one. */
static esp_err_t op_refresh(bsp_display_t *self, bsp_rect_t area, bsp_epd_mode_t mode) {
    (void)area;   /* gate scan always covers the full panel */
    epd_t *s = (epd_t *)self;
    if ((mode & ~BSP_EPD_MODE_FULL) == BSP_EPD_MODE_NONE) return ESP_OK;   /* no waveform */

    bool wake = false;
    xSemaphoreTake(s->mtx, portMAX_DELAY);
    s->pending_mode = mode_coalesce(s->pending_mode, mode);
    s->pending = true;
    if (!s->busy) { s->busy = true; wake = true; }   /* idle->busy: kick the task */
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
    if (s->mtx)  vSemaphoreDelete(s->mtx);
    if (s->done) vSemaphoreDelete(s->done);
    power_off(s);
    free(s->gram);
    free(s->snapshot);
    free(s->disp);
    free(s->row_drv);
    free(s->line[0]);
    free(s->line[1]);
    free(s->hold_line);
    free(s);
    return ESP_OK;
}

// MARK: - create

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
    s->gram     = heap_caps_malloc(fb, MALLOC_CAP_SPIRAM);
    s->snapshot = heap_caps_malloc(fb, MALLOC_CAP_SPIRAM);
    s->disp     = heap_caps_malloc(fb, MALLOC_CAP_SPIRAM);
    s->row_drv  = malloc((size_t)cfg->height * sizeof(bool));
    if (!s->gram || !s->snapshot || !s->disp || !s->row_drv) {
        op_deinit(&s->base);
        return ESP_ERR_NO_MEM;
    }
    memset(s->gram, 0xFF, fb);   /* white target  */
    memset(s->disp, 0xFF, fb);   /* white on glass after the init clear below */

    size_t lb = (cfg->line_bytes + cfg->line_padding + 15) & ~(size_t)15;
    for (int i = 0; i < 2; i++) {
        s->line[i] = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
        if (!s->line[i]) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
        memset(s->line[i], 0, lb);   /* padding region stays zero */
    }
    s->hold_line = heap_caps_aligned_alloc(16, lb, MALLOC_CAP_DMA);
    if (!s->hold_line) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    memset(s->hold_line, 0, lb);   /* all pixels hold (action 0), padding zero */

    ESP_ERROR_CHECK(bus_init(cfg));

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

    /* Establish a known white screen so later refreshes start from a clean state.
     * CLEAR_FULL drives every pixel through the (uniform) clear waveform to white.
     * Synchronous -- runs before the task exists, straight from gram (== disp ==
     * white). */
    do_refresh(s, s->gram, BSP_EPD_MODE_CLEAR_FULL);

    /* Bring up the async waveform engine. Pinned to core 0 to keep its DMA
     * busy-waits off the UI/LVGL core. */
    s->mtx  = xSemaphoreCreateMutex();
    s->done = xSemaphoreCreateBinary();
    if (!s->mtx || !s->done) { op_deinit(&s->base); return ESP_ERR_NO_MEM; }
    if (xTaskCreatePinnedToCore(refresh_task, "epd_refresh", 4096, s, 5, &s->task, 0) != pdPASS) {
        s->task = NULL;
        op_deinit(&s->base);
        return ESP_ERR_NO_MEM;
    }

    *out_display = &s->base;
    return ESP_OK;
}
