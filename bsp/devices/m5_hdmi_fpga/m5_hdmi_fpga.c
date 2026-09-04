/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Command protocol, PLL search and video-timing derivation ported from
 * LovyanGFX (Panel_M5HDMI), Copyright (c) lovyan03, licensed under the
 * FreeBSD License.
 */

#include "m5_hdmi_fpga.h"
#include "m5_hdmi_fpga_jtag.h"
#include "lt8618sx.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "esp_rom_sys.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "m5_hdmi_fpga";

#define CMD_NOP             0x00
#define CMD_READ_ID         0x04
#define CMD_SCREEN_SCALING  0x18
#define CMD_CASET           0x2a
#define CMD_RASET           0x2b
#define CMD_WRITE_RAW_16    0x42
#define CMD_VIDEO_TIMING_V  0xb0
#define CMD_VIDEO_TIMING_H  0xb1
#define CMD_VIDEO_CLOCK     0xb2

#define BASE_CLOCK_HZ  74250000u
#define VCO_TARGET_HZ  800000000
#define CHUNK_PX       (M5_HDMI_FPGA_DMA_CHUNK_BYTES / 2)
/* Poll backs off by one more microsecond each round, so this caps a stuck
 * gateware at roughly half a second rather than hanging the caller. */
#define POLL_MAX       1000
#define NBUF           2

typedef struct {
    uint16_t sync, back_porch, active, front_porch;
} video_timing_t;

typedef struct {
    uint8_t input_divider;
    uint8_t feedback_divider;
    uint8_t output_divider;
    bool    use_half_clock;
} video_clock_t;

typedef struct {
    bsp_display_t base;
    spi_host_device_t spi_host;
    spi_device_handle_t write_dev;
    spi_device_handle_t read_dev;
    gpio_num_t sclk_io, mosi_io, miso_io, cs_io;
    int write_clock_hz, read_clock_hz;

    lt8618sx_t transmitter;
    bsp_display_power_t power;
    bool raw_write_pending;

    float    refresh_rate;
    uint32_t pixel_clock;

    uint16_t *dma_buf[NBUF];
    spi_transaction_t trans[NBUF];
    uint16_t *rotate_buf;
    size_t    rotate_px;
} m5_hdmi_fpga_t;

static inline void cs(m5_hdmi_fpga_t *d, int level) { gpio_set_level(d->cs_io, level); }

static void write_bytes(m5_hdmi_fpga_t *d, const void *data, size_t len) {
    spi_transaction_t t = { .length = len * 8, .tx_buffer = data };
    spi_device_polling_transmit(d->write_dev, &t);
}

/* Read-only (half-duplex) so MOSI stays idle: a status poll issued while the
 * gateware is still taking pixel data must not push more bytes at it. */
static void read_bytes(m5_hdmi_fpga_t *d, void *data, size_t len) {
    spi_transaction_t t = { .length = 0, .rxlength = len * 8, .rx_buffer = data };
    spi_device_polling_transmit(d->read_dev, &t);
}

/* The gateware answers 0x00 on MISO while it is still draining what was sent and
 * a non-zero byte once it is ready. Ends the poll by raising CS. */
static void wait_ready(m5_hdmi_fpga_t *d) {
    uint8_t status = 0;
    uint32_t wait = 0;
    do {
        read_bytes(d, &status, 1);
        if (status == 0) esp_rom_delay_us(++wait);
    } while (status == 0 && wait < POLL_MAX);
    if (status == 0) ESP_LOGW(TAG, "gateware stayed busy");
    cs(d, 1);
}

/* Asserts CS for a command, first draining a preceding pixel stream. That
 * stream is only committed by CS going high, so it must not be polled before
 * then -- the gateware would report busy forever and the tail of the window
 * would stay unwritten. */
static void begin_command(m5_hdmi_fpga_t *d) {
    cs(d, 0);
    if (d->raw_write_pending) {
        d->raw_write_pending = false;
        wait_ready(d);
        cs(d, 0);
    }
}

/* Command packets that carry parameters end in a checksum over the bytes
 * before it. */
static uint8_t checksum(const uint8_t *packet, size_t len) {
    uint8_t sum = 0;
    for (size_t i = 0; i < len; i++) sum += packet[i];
    return ~sum;
}

static void put_be16(uint8_t *dst, uint16_t value) {
    dst[0] = value >> 8;
    dst[1] = value & 0xff;
}

static void send_packet(m5_hdmi_fpga_t *d, const uint8_t *packet, size_t len) {
    begin_command(d);
    write_bytes(d, packet, len);
    wait_ready(d);
}

/* The reply is a run of 0xFF followed by "HD" and the gateware version, but how
 * many 0xFF bytes precede it varies, so the signature is searched for rather
 * than read at a fixed offset: a reply that is off by one byte would otherwise
 * look like a module with no gateware, and the recovery for that erases the
 * FPGA. The leading NOPs and the CS pulse re-synchronize a gateware left
 * mid-command by a host that reset under it. */
static bool read_gateware_id(m5_hdmi_fpga_t *d, uint8_t version[2]) {
    static const uint8_t nops[4] = { CMD_NOP, CMD_NOP, CMD_NOP, CMD_NOP };
    const uint8_t cmd = CMD_READ_ID;
    uint8_t reply[24] = { 0 };

    begin_command(d);
    write_bytes(d, nops, sizeof(nops));
    cs(d, 1);
    write_bytes(d, nops, sizeof(nops));
    cs(d, 0);
    write_bytes(d, &cmd, 1);
    read_bytes(d, reply, sizeof(reply));
    cs(d, 1);

    for (size_t i = 0; i + 3 < sizeof(reply); i++) {
        if (reply[i] != 'H' || reply[i + 1] != 'D') continue;
        if (version) {
            version[0] = reply[i + 2];
            version[1] = reply[i + 3];
        }
        return true;
    }
    return false;
}

/* Divider search for the FPGA's PLL: input clock 74.25 MHz * feedback / input
 * is the pixel clock, and pixel clock * output must land near an 800 MHz VCO. */
static uint32_t pll_params(video_clock_t *vc, uint32_t target_clock) {
    uint32_t fb_clock = BASE_CLOCK_HZ;
    uint32_t save_diff = ~0u;
    uint32_t fb_div = 1;
    uint32_t in_div = BASE_CLOCK_HZ / (target_clock + 1);
    if (in_div == 0) in_div = 1;
    for (;;) {
        uint32_t tmp_clock = fb_clock / in_div;
        uint32_t diff = abs((int32_t)target_clock - (int32_t)tmp_clock);
        if (save_diff > diff) {
            save_diff = diff;
            vc->feedback_divider = fb_div;
            vc->input_divider = in_div;
            if (diff == 0) break;
        }
        if (target_clock < tmp_clock) {
            if (++in_div > 24) break;
        } else {
            if (++fb_div > 64) break;
            fb_clock = BASE_CLOCK_HZ * fb_div;
        }
    }

    uint32_t result = BASE_CLOCK_HZ * vc->feedback_divider / vc->input_divider;

    save_diff = ~0u;
    static const uint8_t odiv_tbl[] = { 2, 4, 8, 16, 32, 48, 64, 80, 96, 112, 128 };
    for (size_t i = 0; i < sizeof(odiv_tbl); i++) {
        uint32_t diff = abs((int32_t)(result * odiv_tbl[i]) - VCO_TARGET_HZ);
        if (save_diff < diff) break;
        save_diff = diff;
        vc->output_divider = odiv_tbl[i];
    }
    vc->use_half_clock = target_clock > BASE_CLOCK_HZ;

    return result;
}

static void send_video_timing(m5_hdmi_fpga_t *d, const video_timing_t *t, uint8_t cmd) {
    uint8_t packet[10];
    packet[0] = cmd;
    put_be16(&packet[1], t->sync);
    put_be16(&packet[3], t->back_porch);
    put_be16(&packet[5], t->active);
    put_be16(&packet[7], t->front_porch);
    packet[9] = checksum(packet, 9);
    send_packet(d, packet, sizeof(packet));
}

static void send_video_clock(m5_hdmi_fpga_t *d, const video_clock_t *vc) {
    uint8_t packet[9] = {
        CMD_VIDEO_CLOCK,
        0, vc->input_divider,
        0, vc->feedback_divider,
        0, vc->output_divider,
        vc->use_half_clock ? 1 : 0,
        0,
    };
    packet[8] = checksum(packet, 8);
    send_packet(d, packet, sizeof(packet));
}

static void send_scaling(m5_hdmi_fpga_t *d, uint8_t x_scale, uint8_t y_scale,
                         uint16_t width, uint16_t height) {
    uint8_t packet[8];
    packet[0] = CMD_SCREEN_SCALING;
    packet[1] = x_scale;
    packet[2] = y_scale;
    put_be16(&packet[3], width);
    put_be16(&packet[5], height);
    packet[7] = checksum(packet, 7);
    send_packet(d, packet, sizeof(packet));
}

/* Derives a blanking layout that hits the requested refresh rate with the pixel
 * clock the PLL can actually make, then programs timing, scaling and clock. */
static esp_err_t init_resolution(m5_hdmi_fpga_t *d) {
    video_clock_t vc = { .input_divider = 2, .feedback_divider = 2, .output_divider = 8 };
    uint32_t output_clock = pll_params(&vc, d->pixel_clock);

    const int half = vc.use_half_clock ? 1 : 0;
    const int32_t total_resolution = (output_clock >> half) / d->refresh_rate;
    const int mem_width = d->base.size.width;
    const int mem_height = d->base.size.height;

    uint32_t diff = ~0u;
    int vert_total = mem_height + 9;
    int hori_total = total_resolution / vert_total;
    int hori_tmp = hori_total, vert_tmp = vert_total;
    const int hori_min = mem_width + ((32 + (mem_width >> 2)) >> half);
    const int hori_max = mem_width + ((768 + (mem_width >> 3)) >> half);
    if (hori_tmp > hori_max) hori_tmp = hori_max;
    for (;;) {
        int d1 = total_resolution - (hori_tmp * vert_tmp);
        uint32_t diff_abs = abs(d1);
        if (diff > diff_abs) {
            diff = diff_abs;
            hori_total = hori_tmp;
            vert_total = vert_tmp;
            if (diff == 0) break;
        }
        if (d1 >= 0) {
            ++vert_tmp;
        } else if (--hori_tmp < hori_min) {
            break;
        }
    }

    /* Too little blanking left: the mode cannot be generated. */
    if (hori_total <= hori_min) {
        ESP_LOGE(TAG, "%dx%d @ %.2f Hz out of range", mem_width, mem_height,
                 (double)d->refresh_rate);
        return ESP_ERR_INVALID_ARG;
    }

    video_timing_t vert = { .active = mem_height, .sync = 1 };
    uint32_t remain = vert_total - mem_height - vert.sync;
    vert.front_porch = remain >> 1;
    vert.back_porch = remain - vert.front_porch;

    video_timing_t hori = { .active = mem_width };
    remain = hori_total - mem_width;
    hori.sync = 24 + (remain >> 8);
    remain -= hori.sync;
    hori.front_porch = (remain * 131) >> 8;
    hori.back_porch = remain - hori.front_porch;

    send_video_timing(d, &vert, CMD_VIDEO_TIMING_V);
    send_video_timing(d, &hori, CMD_VIDEO_TIMING_H);
    send_scaling(d, 1, 1, mem_width, mem_height);
    send_video_clock(d, &vc);

    ESP_LOGI(TAG, "%dx%d @ %.2f Hz, pixel clock %u Hz (pll fb:%u in:%u out:%u)",
             mem_width, mem_height, (double)d->refresh_rate, (unsigned)output_clock,
             vc.feedback_divider, vc.input_divider, vc.output_divider);
    return ESP_OK;
}

static void set_window(m5_hdmi_fpga_t *d, int x0, int y0, int x1, int y1) {
    uint8_t packet[11];
    packet[0] = CMD_CASET;
    put_be16(&packet[1], x0);
    put_be16(&packet[3], x1);
    packet[5] = CMD_RASET;
    put_be16(&packet[6], y0);
    put_be16(&packet[8], y1);
    packet[10] = CMD_WRITE_RAW_16;
    write_bytes(d, packet, sizeof(packet));
}

/* Streams RGB565 pixels big-endian (the gateware's order) through DMA bounce
 * buffers, overlapping the byte swap of one chunk with the transfer of the
 * previous one. */
static void stream_pixels(m5_hdmi_fpga_t *d, const uint16_t *px, size_t count) {
    size_t queued = 0, idx = 0;
    int bi = 0;
    while (idx < count) {
        if (queued == NBUF) {
            spi_transaction_t *done;
            spi_device_get_trans_result(d->write_dev, &done, portMAX_DELAY);
            queued--;
        }
        size_t n = count - idx < CHUNK_PX ? count - idx : CHUNK_PX;
        uint16_t *buf = d->dma_buf[bi];
        for (size_t i = 0; i < n; i++) buf[i] = __builtin_bswap16(px[idx + i]);
        d->trans[bi] = (spi_transaction_t){ .length = n * 16, .tx_buffer = buf };
        spi_device_queue_trans(d->write_dev, &d->trans[bi], portMAX_DELAY);
        queued++;
        bi ^= 1;
        idx += n;
    }
    while (queued) {
        spi_transaction_t *done;
        spi_device_get_trans_result(d->write_dev, &done, portMAX_DELAY);
        queued--;
    }
}

/* Writes black pixels rather than using the gateware's fill command: a fill
 * covering the whole screen reports complete while it is still running inside
 * the FPGA, and pixel writes issued into that window are swallowed. Streaming
 * black takes a full frame time but is honest about when it is done. */
static esp_err_t clear(bsp_display_t *self) {
    m5_hdmi_fpga_t *d = (m5_hdmi_fpga_t *)self;
    memset(d->dma_buf[0], 0, M5_HDMI_FPGA_DMA_CHUNK_BYTES);

    begin_command(d);
    set_window(d, 0, 0, d->base.size.width - 1, d->base.size.height - 1);
    size_t left = (size_t)d->base.size.width * d->base.size.height;
    while (left) {
        size_t n = left < CHUNK_PX ? left : CHUNK_PX;
        spi_transaction_t t = { .length = n * 16, .tx_buffer = d->dma_buf[0] };
        spi_device_polling_transmit(d->write_dev, &t);
        left -= n;
    }
    cs(d, 1);
    d->raw_write_pending = true;
    return ESP_OK;
}

static esp_err_t draw_bitmap(bsp_display_t *self, bsp_rect_t area, const void *pixels,
                             bsp_rotation_t rotation) {
    m5_hdmi_fpga_t *d = (m5_hdmi_fpga_t *)self;
    const int w = area.size.width, h = area.size.height;
    if (w <= 0 || h <= 0) return ESP_OK;

    const uint16_t *src = pixels;
    if (rotation != BSP_ROTATION_0) {
        size_t need = (size_t)w * h;
        if (d->rotate_px < need) {
            free(d->rotate_buf);
            d->rotate_buf = malloc(need * 2);
            d->rotate_px = d->rotate_buf ? need : 0;
        }
        if (!d->rotate_buf) return ESP_ERR_NO_MEM;
        bsp_blit_rotated((uint8_t *)d->rotate_buf, w, 2,
                         (bsp_rect_t){ {0, 0}, { w, h } }, pixels, rotation);
        src = d->rotate_buf;
    }

    begin_command(d);
    set_window(d, area.origin.x, area.origin.y,
               area.origin.x + w - 1, area.origin.y + h - 1);
    stream_pixels(d, src, (size_t)w * h);
    cs(d, 1);
    d->raw_write_pending = true;
    return ESP_OK;
}

/* The module has no backlight and no panel rail to cut, so SLEEP and OFF both
 * stop the transmitter's output; the gateware keeps its frame buffer either way
 * and ON re-runs the transmitter bring-up. */
static esp_err_t set_power(bsp_display_t *self, bsp_display_power_t state) {
    m5_hdmi_fpga_t *d = (m5_hdmi_fpga_t *)self;
    if (state == d->power) return ESP_OK;

    esp_err_t err = (state == BSP_DISPLAY_POWER_ON)
        ? lt8618sx_init(d->transmitter)
        : lt8618sx_reset(d->transmitter);
    if (err != ESP_OK) return err;

    d->power = state;
    return ESP_OK;
}

static esp_err_t deinit(bsp_display_t *self) {
    m5_hdmi_fpga_t *d = (m5_hdmi_fpga_t *)self;
    if (d->transmitter) lt8618sx_delete(d->transmitter);
    if (d->write_dev) spi_bus_remove_device(d->write_dev);
    if (d->read_dev) spi_bus_remove_device(d->read_dev);
    spi_bus_free(d->spi_host);
    for (int i = 0; i < NBUF; i++) heap_caps_free(d->dma_buf[i]);
    free(d->rotate_buf);
    free(d);
    return ESP_OK;
}

static esp_err_t spi_start(m5_hdmi_fpga_t *d) {
    const spi_bus_config_t bus_cfg = {
        .sclk_io_num     = d->sclk_io,
        .mosi_io_num     = d->mosi_io,
        .miso_io_num     = d->miso_io,
        .quadwp_io_num   = -1,
        .quadhd_io_num   = -1,
        .max_transfer_sz = M5_HDMI_FPGA_DMA_CHUNK_BYTES,
    };
    esp_err_t err = spi_bus_initialize(d->spi_host, &bus_cfg, SPI_DMA_CH_AUTO);
    if (err != ESP_OK) return err;

    /* CS is ours (see the header), so both devices leave it unmapped; they
     * differ only in clock — the gateware answers reads far slower than it
     * accepts writes. */
    spi_device_interface_config_t dev_cfg = {
        .clock_speed_hz = d->write_clock_hz,
        .mode           = 3,
        .spics_io_num   = -1,
        .queue_size     = NBUF,
    };
    err = spi_bus_add_device(d->spi_host, &dev_cfg, &d->write_dev);
    if (err != ESP_OK) return err;

    dev_cfg.clock_speed_hz = d->read_clock_hz;
    dev_cfg.queue_size = 1;
    dev_cfg.flags = SPI_DEVICE_HALFDUPLEX;
    return spi_bus_add_device(d->spi_host, &dev_cfg, &d->read_dev);
}

static void spi_stop(m5_hdmi_fpga_t *d) {
    if (d->write_dev) spi_bus_remove_device(d->write_dev);
    if (d->read_dev) spi_bus_remove_device(d->read_dev);
    d->write_dev = NULL;
    d->read_dev = NULL;
    spi_bus_free(d->spi_host);
}

/* A module that was just powered up has an empty FPGA SRAM and answers nothing
 * on SPI. Loading the gateware bit-bangs the same four pins as JTAG, so the SPI
 * peripheral has to let go of them for the duration. */
static esp_err_t load_gateware(m5_hdmi_fpga_t *d) {
    spi_stop(d);
    esp_err_t err = m5_hdmi_fpga_jtag_load(d->sclk_io, d->mosi_io, d->miso_io, d->cs_io);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "no gateware in the FPGA and none loaded: %s", esp_err_to_name(err));
        return err;
    }
    if ((err = spi_start(d)) != ESP_OK) return err;

    /* The freshly configured gateware takes a second or so to start answering. */
    for (int retry = 500; retry; retry--) {
        vTaskDelay(pdMS_TO_TICKS(10));
        if (read_gateware_id(d, NULL)) return ESP_OK;
    }
    ESP_LOGE(TAG, "FPGA did not answer after loading the gateware");
    return ESP_ERR_TIMEOUT;
}

/* Halves the clocks until an ID comes back, in case the wiring cannot carry the
 * configured rate. */
static esp_err_t sync_clock(m5_hdmi_fpga_t *d) {
    uint8_t version[2] = { 0, 0 };
    for (int retry = 8; retry; retry--) {
        if (read_gateware_id(d, version)) {
            ESP_LOGI(TAG, "gateware version %u.%u", version[0], version[1]);
            return ESP_OK;
        }

        d->write_clock_hz /= 2;
        d->read_clock_hz /= 2;
        if (d->read_clock_hz < 1000000) break;

        spi_stop(d);
        esp_err_t err = spi_start(d);
        if (err != ESP_OK) return err;
        ESP_LOGW(TAG, "retrying at write %d Hz / read %d Hz",
                 d->write_clock_hz, d->read_clock_hz);
    }
    return ESP_ERR_NOT_FOUND;
}

esp_err_t m5_hdmi_fpga_create(const m5_hdmi_fpga_config_t *config, bsp_display_t **out) {
    m5_hdmi_fpga_t *d = calloc(1, sizeof(*d));
    if (!d) return ESP_ERR_NO_MEM;

    d->spi_host = config->spi_host;
    d->sclk_io  = config->sclk_io;
    d->mosi_io  = config->mosi_io;
    d->miso_io  = config->miso_io;
    d->cs_io    = config->cs_io;
    d->write_clock_hz = config->write_clock_hz ? config->write_clock_hz
                                               : M5_HDMI_FPGA_WRITE_CLOCK_HZ;
    d->read_clock_hz  = config->read_clock_hz ? config->read_clock_hz
                                              : M5_HDMI_FPGA_READ_CLOCK_HZ;
    d->pixel_clock = config->pixel_clock ? config->pixel_clock : M5_HDMI_FPGA_PIXEL_CLOCK_HZ;
    d->power = BSP_DISPLAY_POWER_ON;

    d->base.type   = BSP_DISPLAY_TYPE_SPI;
    d->base.format = BSP_PIXEL_FORMAT_RGB565;
    d->base.size   = (config->size.width > 0 && config->size.height > 0)
        ? config->size
        : (bsp_size_t){ 1280, 720 };

    d->refresh_rate = config->refresh_rate;
    if (d->refresh_rate < 1.0f) {
        int pixels = d->base.size.width * d->base.size.height;
        d->refresh_rate = pixels > 1843200 ? 24.0f : (pixels > 1024000 ? 30.0f : 60.0f);
    }

    const gpio_config_t cs_cfg = {
        .pin_bit_mask = 1ULL << d->cs_io,
        .mode         = GPIO_MODE_OUTPUT,
    };
    gpio_config(&cs_cfg);
    cs(d, 1);

    for (int i = 0; i < NBUF; i++) {
        d->dma_buf[i] = heap_caps_malloc(M5_HDMI_FPGA_DMA_CHUNK_BYTES, MALLOC_CAP_DMA);
        if (!d->dma_buf[i]) { deinit(&d->base); return ESP_ERR_NO_MEM; }
    }

    esp_err_t err = lt8618sx_create(config->i2c_bus, config->i2c_address, &d->transmitter);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "lt8618sx_create: %s", esp_err_to_name(err));
        deinit(&d->base);
        return err;
    }

    /* Three identical ID bytes mean the transmitter never drove the bus, i.e.
     * no module on this port. */
    uint8_t id[3] = { 0, 0, 0 };
    err = lt8618sx_read_chip_id(d->transmitter, id);
    if (err != ESP_OK || (id[0] == id[1] && id[0] == id[2])) {
        ESP_LOGW(TAG, "no HDMI transmitter (id %02x %02x %02x)", id[0], id[1], id[2]);
        deinit(&d->base);
        return ESP_ERR_NOT_FOUND;
    }
    lt8618sx_reset(d->transmitter);

    if ((err = spi_start(d)) != ESP_OK) {
        ESP_LOGE(TAG, "spi_start: %s", esp_err_to_name(err));
        deinit(&d->base);
        return err;
    }

    /* Retry before falling back: a gateware that is alive but out of step
     * answers on a later try, and the fallback would erase it. */
    bool present = false;
    for (int retry = 3; retry && !present; retry--) present = read_gateware_id(d, NULL);
    if (!present && (err = load_gateware(d)) != ESP_OK) {
        deinit(&d->base);
        return err;
    }
    if ((err = sync_clock(d)) != ESP_OK) {
        ESP_LOGE(TAG, "FPGA did not answer with an ID");
        deinit(&d->base);
        return err;
    }

    if ((err = init_resolution(d)) != ESP_OK) {
        deinit(&d->base);
        return err;
    }

    if ((err = lt8618sx_init(d->transmitter)) != ESP_OK) {
        ESP_LOGE(TAG, "transmitter init: %s", esp_err_to_name(err));
        deinit(&d->base);
        return err;
    }

    d->base.draw_bitmap = draw_bitmap;
    d->base.deinit      = deinit;
    d->base.set_power   = set_power;
    d->base.clear       = clear;

    ESP_LOGI(TAG, "HDMI module ready");
    *out = &d->base;
    return ESP_OK;
}
