/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * IT8951E e-paper TCON driver (SPI mode).
 */

#include "it8951e.h"

#include <stdlib.h>
#include <string.h>

#include "esp_check.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "it8951e";

/* ---- SPI preamble words ------------------------------------------------- */
#define PRE_CMD          0x6000u
#define PRE_WRITE        0x0000u
#define PRE_READ         0x1000u

/* ---- IT8951 commands ---------------------------------------------------- */
#define CMD_SYS_RUN      0x0001u
#define CMD_STANDBY      0x0002u
#define CMD_SLEEP        0x0003u
#define CMD_REG_RD       0x0010u
#define CMD_REG_WR       0x0011u
#define CMD_LD_IMG_AREA  0x0021u
#define CMD_LD_IMG_END   0x0022u
#define CMD_DPY_AREA     0x0034u
#define CMD_DPY_BUF_AREA 0x0037u
#define CMD_VCOM         0x0039u
#define CMD_GET_DEV_INFO 0x0302u

/* ---- Registers ---------------------------------------------------------- */
#define REG_SYS_BASE     0x0000u
#define REG_I80CPCR      (REG_SYS_BASE + 0x04u)

#define REG_MCSR_BASE    0x0200u
#define REG_LISAR        (REG_MCSR_BASE + 0x08u) /* image base addr LSW @+0, MSW @+2 */

#define REG_DISP_BASE    0x1000u
#define REG_LUTAFSR      (REG_DISP_BASE + 0x224u) /* 0 == idle */

/* ---- Timing ------------------------------------------------------------- */
#define HRDY_TIMEOUT_MS  3000u
#define RESET_LOW_US     100
#define RESET_HIGH_MS    100

/* x/w alignment in pixels for the given bpp, so the byte stream is whole. */
static inline uint16_t bpp_x_align(it8951e_bpp_t bpp) {
    switch (bpp) {
        case IT8951E_BPP_2: return 8; /* 8 px = 16 bits */
        case IT8951E_BPP_3: return 16;
        case IT8951E_BPP_4: return 4; /* 4 px = 16 bits */
        case IT8951E_BPP_8: return 2;
    }
    return 4;
}

static inline uint8_t bpp_bits(it8951e_bpp_t bpp) {
    switch (bpp) {
        case IT8951E_BPP_2: return 2;
        case IT8951E_BPP_3: return 3;
        case IT8951E_BPP_4: return 4;
        case IT8951E_BPP_8: return 8;
    }
    return 4;
}

typedef struct it8951e_dev it8951e_dev_t;
struct it8951e_dev {
    it8951e_config_t     cfg;
    spi_device_handle_t  spi;        /* write/command device (fast clock)      */
    spi_device_handle_t  spi_rd;     /* register-read device (slow clock)      */
    it8951e_panel_info_t info;
    bool                 info_valid;
    int                  bus_depth;  /* >0 = bus held across packets (nestable) */
};

/* ========================================================================
 * SPI primitives
 *
 * CS handling: the SPI driver toggles CS for us, but we need CS to stay low
 * across "preamble -> wait_hrdy -> word(s)" sequences. We hold it active by
 * acquiring the bus and using SPI_TRANS_CS_KEEP_ACTIVE on every transaction
 * except the last in a group. The bus acquire also serializes against any
 * other device (e.g. the future microSD reader) sharing the same host.
 * ===================================================================== */

/* CS is bit-banged (see it8951e_create); held low for one packet's worth of
 * transfers — preamble, the HRDY wait, then the data words. */
static inline void cs_set(it8951e_dev_t *dev, int level) {
    gpio_set_level(dev->cfg.cs_io, level);
}

static esp_err_t wait_hrdy(it8951e_dev_t *dev, uint32_t timeout_ms) {
    if (gpio_get_level(dev->cfg.busy_io)) return ESP_OK;

    int64_t deadline = esp_timer_get_time() + (int64_t)timeout_ms * 1000;
    while (gpio_get_level(dev->cfg.busy_io) == 0) {
        if (esp_timer_get_time() > deadline) {
            ESP_LOGE(TAG, "HRDY timeout (%u ms)", (unsigned)timeout_ms);
            return ESP_ERR_TIMEOUT;
        }
        /* HRDY can pulse low for tens of microseconds for short ops, or for
         * tens of milliseconds during display refresh. Use a short busy spin
         * for the common-fast case, then yield. */
        if (esp_timer_get_time() - (deadline - (int64_t)timeout_ms * 1000) < 500) {
            esp_rom_delay_us(10);
        } else {
            vTaskDelay(1);
        }
    }
    return ESP_OK;
}

static esp_err_t spi_write_bytes(spi_device_handle_t io,
                                 const uint8_t *tx, size_t len,
                                 bool keep_cs) {
    spi_transaction_t t = {
        .flags     = keep_cs ? SPI_TRANS_CS_KEEP_ACTIVE : 0,
        .length    = len * 8,
        .tx_buffer = tx,
    };
    return spi_device_polling_transmit(io, &t);
}

static esp_err_t spi_read_bytes(spi_device_handle_t io,
                                uint8_t *rx, size_t len,
                                bool keep_cs) {
    spi_transaction_t t = {
        .flags     = keep_cs ? SPI_TRANS_CS_KEEP_ACTIVE : 0,
        .length    = len * 8,
        .rxlength  = len * 8,
        .rx_buffer = rx,
    };
    return spi_device_polling_transmit(io, &t);
}

static inline esp_err_t spi_write_u16(spi_device_handle_t io, uint16_t v, bool keep_cs) {
    uint8_t buf[2] = { (uint8_t)(v >> 8), (uint8_t)v };
    return spi_write_bytes(io, buf, 2, keep_cs);
}

/* ---- IT8951 packet primitives ------------------------------------------ */

/* While the bus is held (it8951e_bus_acquire / a wrapped primitive), each packet
 * skips the per-packet acquire/release so an SD transaction can't slip between
 * the packets of one logical operation on the shared bus. */
static esp_err_t io_begin(it8951e_dev_t *dev) {
    if (dev->bus_depth) return wait_hrdy(dev, HRDY_TIMEOUT_MS);
    esp_err_t err = spi_device_acquire_bus(dev->spi, portMAX_DELAY);
    if (err != ESP_OK) return err;
    err = wait_hrdy(dev, HRDY_TIMEOUT_MS);
    if (err != ESP_OK) spi_device_release_bus(dev->spi);
    return err;
}

static void io_end(it8951e_dev_t *dev) {
    if (!dev->bus_depth) spi_device_release_bus(dev->spi);
}

static esp_err_t pkt_write_cmd(it8951e_dev_t *dev, uint16_t cmd) {
    esp_err_t err = io_begin(dev);
    if (err != ESP_OK) return err;
    cs_set(dev, 0);
    err = spi_write_u16(dev->spi, PRE_CMD, true);
    if (err == ESP_OK) err = wait_hrdy(dev, HRDY_TIMEOUT_MS);
    if (err == ESP_OK) err = spi_write_u16(dev->spi, cmd, false);
    cs_set(dev, 1);
    io_end(dev);
    return err;
}

static esp_err_t pkt_write_data(it8951e_dev_t *dev,
                                const uint16_t *args, size_t n_args) {
    if (n_args == 0) return ESP_OK;
    esp_err_t err = io_begin(dev);
    if (err != ESP_OK) return err;
    cs_set(dev, 0);
    err = spi_write_u16(dev->spi, PRE_WRITE, true);
    /* Re-check HRDY before every parameter word: the controller can de-assert it
     * mid-list, and overrunning its input FIFO freezes the display engine. */
    for (size_t i = 0; i < n_args && err == ESP_OK; i++) {
        err = wait_hrdy(dev, HRDY_TIMEOUT_MS);
        if (err == ESP_OK) err = spi_write_u16(dev->spi, args[i], true);
    }
    cs_set(dev, 1);
    io_end(dev);
    return err;
}

/* Reads run on the slow-clock read device with their own standalone bus
 * acquire: the read preamble + dummy + data must stay CS-contiguous on one
 * device handle, so the whole read (including its PRE_READ) goes through spi_rd.
 * Reads are never nested inside a held write-bus, so no bus_depth bookkeeping. */
static esp_err_t pkt_read_data(it8951e_dev_t *dev,
                               uint16_t *out, size_t n_words) {
    if (n_words == 0) return ESP_OK;
    esp_err_t err = spi_device_acquire_bus(dev->spi_rd, portMAX_DELAY);
    if (err != ESP_OK) return err;

    cs_set(dev, 0);
    err = wait_hrdy(dev, HRDY_TIMEOUT_MS);
    if (err == ESP_OK) err = spi_write_u16(dev->spi_rd, PRE_READ, true);
    if (err == ESP_OK) err = wait_hrdy(dev, HRDY_TIMEOUT_MS);
    /* Dummy word — protocol requires one read before real data. */
    uint8_t dummy[2];
    if (err == ESP_OK) err = spi_read_bytes(dev->spi_rd, dummy, 2, true);
    if (err == ESP_OK) err = wait_hrdy(dev, HRDY_TIMEOUT_MS);

    /* Read n_words * 2 bytes, byte-swap into host-endian uint16. Use a
     * stack chunk buffer. */
    enum { CHUNK = 256 };
    uint8_t buf[CHUNK * 2];
    for (size_t i = 0; i < n_words && err == ESP_OK; ) {
        size_t take = (n_words - i < CHUNK) ? (n_words - i) : CHUNK;
        bool last_chunk = (i + take == n_words);
        err = spi_read_bytes(dev->spi_rd, buf, take * 2, !last_chunk);
        if (err != ESP_OK) break;
        for (size_t j = 0; j < take; j++) {
            out[i + j] = ((uint16_t)buf[2 * j] << 8) | buf[2 * j + 1];
        }
        i += take;
    }
    cs_set(dev, 1);
    spi_device_release_bus(dev->spi_rd);
    return err;
}

/* ---- Convenience wrappers for cmd + args ------------------------------- */

/* cmd_with_args spans multiple write packets; hold the bus across them so the
 * cmd/args sub-packets stay contiguous against an SD device on the same bus. The
 * hold is nestable, so it composes when a caller already holds the bus (e.g.
 * op_refresh) and self-protects the standalone polls (wait_idle). */
static esp_err_t cmd_with_args(it8951e_dev_t *dev, uint16_t cmd,
                               const uint16_t *args, size_t n_args) {
    esp_err_t err = it8951e_bus_acquire(dev);
    if (err != ESP_OK) return err;
    err = pkt_write_cmd(dev, cmd);
    if (err == ESP_OK && n_args > 0) err = pkt_write_data(dev, args, n_args);
    it8951e_bus_release(dev);
    return err;
}

/* No outer bus hold across the command and the read: the read takes its own
 * acquire on the slow-clock read device, which would deadlock under a hold on
 * the write device. */
static esp_err_t reg_read(it8951e_dev_t *dev, uint16_t reg, uint16_t *out) {
    uint16_t arg = reg;
    esp_err_t err = cmd_with_args(dev, CMD_REG_RD, &arg, 1);
    if (err == ESP_OK) err = pkt_read_data(dev, out, 1);
    return err;
}

static esp_err_t reg_write(it8951e_dev_t *dev, uint16_t reg, uint16_t value) {
    uint16_t args[2] = { reg, value };
    return cmd_with_args(dev, CMD_REG_WR, args, 2);
}

/* ========================================================================
 * Public API
 * ===================================================================== */

esp_err_t it8951e_bus_acquire(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    if (handle->bus_depth > 0) { handle->bus_depth++; return ESP_OK; }
    esp_err_t err = spi_device_acquire_bus(handle->spi, portMAX_DELAY);
    if (err == ESP_OK) handle->bus_depth = 1;
    return err;
}

esp_err_t it8951e_bus_release(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    if (handle->bus_depth == 0) return ESP_OK;
    if (--handle->bus_depth == 0) spi_device_release_bus(handle->spi);
    return ESP_OK;
}

esp_err_t it8951e_read_reg(it8951e_handle_t handle, uint16_t reg, uint16_t *out_value) {
    ESP_RETURN_ON_FALSE(handle && out_value, ESP_ERR_INVALID_ARG, TAG, "null arg");
    return reg_read(handle, reg, out_value);
}

esp_err_t it8951e_write_reg(it8951e_handle_t handle, uint16_t reg, uint16_t value) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    return reg_write(handle, reg, value);
}

esp_err_t it8951e_sys_run(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    return pkt_write_cmd(handle, CMD_SYS_RUN);
}

esp_err_t it8951e_standby(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    return pkt_write_cmd(handle, CMD_STANDBY);
}

esp_err_t it8951e_sleep(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    return pkt_write_cmd(handle, CMD_SLEEP);
}

esp_err_t it8951e_wait_idle(it8951e_handle_t handle, uint32_t timeout_ms) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");

    /* A full-panel high-quality refresh holds HRDY low for seconds — longer than
     * a single packet's HRDY_TIMEOUT_MS. Wait for the controller to accept SPI
     * again on the caller's full budget first (GPIO only, no bus), so a slow
     * refresh isn't misreported as a 3 s HRDY timeout by the LUTAFSR poll's own
     * per-packet wait. */
    esp_err_t err = wait_hrdy(handle, timeout_ms);
    if (err != ESP_OK) return err;

    int64_t deadline = esp_timer_get_time() + (int64_t)timeout_ms * 1000;
    while (1) {
        uint16_t v = 0xffff;
        err = reg_read(handle, REG_LUTAFSR, &v);
        if (err != ESP_OK) return err;
        if (v == 0) return ESP_OK;
        if (timeout_ms == 0) return ESP_ERR_TIMEOUT;
        if (timeout_ms != UINT32_MAX && esp_timer_get_time() > deadline) {
            return ESP_ERR_TIMEOUT;
        }
        vTaskDelay(pdMS_TO_TICKS(5));
    }
}

esp_err_t it8951e_get_panel_info(it8951e_handle_t handle, it8951e_panel_info_t *out_info) {
    ESP_RETURN_ON_FALSE(handle && out_info, ESP_ERR_INVALID_ARG, TAG, "null arg");
    if (handle->info_valid) {
        *out_info = handle->info;
        return ESP_OK;
    }

    esp_err_t err = pkt_write_cmd(handle, CMD_GET_DEV_INFO);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(1));

    uint16_t buf[20] = {0};
    err = pkt_read_data(handle, buf, 20);
    if (err != ESP_OK) return err;

    it8951e_panel_info_t info = {0};
    info.panel_w    = buf[0];
    info.panel_h    = buf[1];
    info.mem_addr_l = buf[2];
    info.mem_addr_h = buf[3];

    /* Sanity check: if the address came back as 0 or all-ones, the read was
     * corrupt. Fall back to a known-good image-buffer address. */
    uint32_t mem_addr = ((uint32_t)info.mem_addr_h << 16) | info.mem_addr_l;
    if (mem_addr == 0 || mem_addr == 0xFFFFFFFFu) {
        ESP_LOGW(TAG, "bad mem_addr 0x%08x from GET_DEV_INFO, using default",
                 (unsigned)mem_addr);
        info.mem_addr_l = 0x36E0;
        info.mem_addr_h = 0x0012;
    }
    /* Version strings come back as 8 big-endian 16-bit words; copy as bytes
     * in transmit order. */
    for (size_t i = 0; i < 8; i++) {
        info.fw_version[2 * i + 0]  = (char)(buf[4 + i] >> 8);
        info.fw_version[2 * i + 1]  = (char)(buf[4 + i] & 0xff);
        info.lut_version[2 * i + 0] = (char)(buf[12 + i] >> 8);
        info.lut_version[2 * i + 1] = (char)(buf[12 + i] & 0xff);
    }
    info.fw_version[15]  = '\0';
    info.lut_version[15] = '\0';

    handle->info       = info;
    handle->info_valid = true;
    *out_info          = info;

    ESP_LOGI(TAG, "panel %ux%u, mem 0x%04x%04x, fw=%s lut=%s",
             info.panel_w, info.panel_h, info.mem_addr_h, info.mem_addr_l,
             info.fw_version, info.lut_version);
    return ESP_OK;
}

esp_err_t it8951e_set_vcom(it8951e_handle_t handle, float volts) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    /* VCOM is negative; arg is magnitude in mV. */
    int mv = (int)(volts < 0 ? -volts * 1000.0f : volts * 1000.0f);
    if (mv < 0 || mv > 5000) return ESP_ERR_INVALID_ARG;
    uint16_t args[2] = { 1, (uint16_t)mv };
    return cmd_with_args(handle, CMD_VCOM, args, 2);
}

esp_err_t it8951e_get_vcom(it8951e_handle_t handle, float *out_volts) {
    ESP_RETURN_ON_FALSE(handle && out_volts, ESP_ERR_INVALID_ARG, TAG, "null arg");
    uint16_t arg = 0;
    esp_err_t err = cmd_with_args(handle, CMD_VCOM, &arg, 1);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(1));
    uint16_t mv = 0;
    err = pkt_read_data(handle, &mv, 1);
    if (err != ESP_OK) return err;
    *out_volts = -(float)mv / 1000.0f;
    return ESP_OK;
}

/* ---- Drawing ----------------------------------------------------------- */

static esp_err_t set_target_memory(it8951e_handle_t handle,
                                   uint16_t addr_l, uint16_t addr_h) {
    esp_err_t err = reg_write(handle, REG_LISAR + 2, addr_h);
    if (err == ESP_OK) err = reg_write(handle, REG_LISAR + 0, addr_l);
    return err;
}

esp_err_t it8951e_load_image(it8951e_handle_t handle,
                             const it8951e_area_t *area,
                             const it8951e_image_t *image) {
    ESP_RETURN_ON_FALSE(handle && area && image && image->data,
                        ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(handle->info_valid, ESP_ERR_INVALID_STATE, TAG,
                        "panel info not fetched");

    uint16_t x_align = bpp_x_align(image->bpp);
    if ((area->x % x_align) || (area->w % x_align)) {
        ESP_LOGE(TAG, "x=%u w=%u must align to %u for this bpp",
                 area->x, area->w, x_align);
        return ESP_ERR_INVALID_ARG;
    }

    size_t expected = ((size_t)area->w * area->h * bpp_bits(image->bpp)) / 8;
    if (image->data_size != expected) {
        ESP_LOGE(TAG, "data_size=%u != expected=%u",
                 (unsigned)image->data_size, (unsigned)expected);
        return ESP_ERR_INVALID_SIZE;
    }

    esp_err_t err = set_target_memory(handle,
                                      handle->info.mem_addr_l,
                                      handle->info.mem_addr_h);
    if (err != ESP_OK) return err;

    uint16_t arg0 = ((uint16_t)image->endian << 8) |
                    ((uint16_t)image->bpp << 4) |
                    (uint16_t)image->rotation;
    uint16_t args[5] = { arg0, area->x, area->y, area->w, area->h };
    err = cmd_with_args(handle, CMD_LD_IMG_AREA, args, 5);
    if (err != ESP_OK) return err;

    /* Stream the pixel bytes in chunks under the SPI host's max_transfer_sz.
     * data_size is even by construction (w*h*bpp_bits / 8 with the alignment
     * checked above). */
    err = io_begin(handle);
    if (err != ESP_OK) return err;
    cs_set(handle, 0);
    err = spi_write_u16(handle->spi, PRE_WRITE, true);
    if (err == ESP_OK) err = wait_hrdy(handle, HRDY_TIMEOUT_MS);
    if (err == ESP_OK) {
        enum { CHUNK = 4096 };
        const uint8_t *p = (const uint8_t *)image->data;
        size_t remaining = image->data_size;
        while (remaining > 0 && err == ESP_OK) {
            size_t take = remaining > CHUNK ? CHUNK : remaining;
            bool last = (take == remaining);
            err = spi_write_bytes(handle->spi, p, take, !last);
            p += take;
            remaining -= take;
        }
    }
    cs_set(handle, 1);
    io_end(handle);
    if (err != ESP_OK) return err;

    return pkt_write_cmd(handle, CMD_LD_IMG_END);
}

esp_err_t it8951e_display(it8951e_handle_t handle,
                          const it8951e_area_t *area,
                          it8951e_mode_t mode) {
    ESP_RETURN_ON_FALSE(handle && area, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(handle->info_valid, ESP_ERR_INVALID_STATE, TAG,
                        "panel info not fetched");

    uint16_t args[7] = {
        area->x, area->y, area->w, area->h,
        (uint16_t)mode,
        handle->info.mem_addr_l,
        handle->info.mem_addr_h,
    };
    return cmd_with_args(handle, CMD_DPY_BUF_AREA, args, 7);
}

esp_err_t it8951e_flush(it8951e_handle_t handle,
                        const it8951e_area_t *area,
                        const it8951e_image_t *image,
                        it8951e_mode_t mode) {
    /* Refreshing while a previous refresh is in flight is undefined — wait
     * for LUTAFSR == 0 first. Callers that batch loads can skip this by
     * using load_image + display directly. */
    esp_err_t err = it8951e_wait_idle(handle, HRDY_TIMEOUT_MS);
    if (err != ESP_OK) return err;
    err = it8951e_load_image(handle, area, image);
    if (err != ESP_OK) return err;
    return it8951e_display(handle, area, mode);
}

esp_err_t it8951e_clear(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(handle->info_valid, ESP_ERR_INVALID_STATE, TAG,
                        "panel info not fetched");

    const uint16_t w = handle->info.panel_w;
    const uint16_t h = handle->info.panel_h;
    /* Stream white (0xff) without holding the full framebuffer in RAM. */
    esp_err_t err = set_target_memory(handle,
                                      handle->info.mem_addr_l,
                                      handle->info.mem_addr_h);
    if (err != ESP_OK) return err;

    uint16_t arg0 = ((uint16_t)IT8951E_ENDIAN_LITTLE << 8) |
                    ((uint16_t)IT8951E_BPP_4 << 4) |
                    (uint16_t)IT8951E_ROT_0;
    uint16_t args[5] = { arg0, 0, 0, w, h };
    err = cmd_with_args(handle, CMD_LD_IMG_AREA, args, 5);
    if (err != ESP_OK) return err;

    err = io_begin(handle);
    if (err != ESP_OK) return err;
    cs_set(handle, 0);
    err = spi_write_u16(handle->spi, PRE_WRITE, true);
    if (err == ESP_OK) err = wait_hrdy(handle, HRDY_TIMEOUT_MS);

    if (err == ESP_OK) {
        enum { CHUNK = 512 };
        static uint8_t white[CHUNK];
        memset(white, 0xff, sizeof(white));
        size_t remaining = ((size_t)w * h) / 2; /* 4bpp => 2 px per byte */
        while (remaining > 0 && err == ESP_OK) {
            size_t take = remaining > CHUNK ? CHUNK : remaining;
            bool last = (take == remaining);
            err = spi_write_bytes(handle->spi, white, take, !last);
            remaining -= take;
        }
    }
    cs_set(handle, 1);
    io_end(handle);
    if (err != ESP_OK) return err;

    err = pkt_write_cmd(handle, CMD_LD_IMG_END);
    if (err != ESP_OK) return err;

    it8951e_area_t full = { 0, 0, w, h };
    return it8951e_display(handle, &full, IT8951E_MODE_INIT);
}

/* ---- Lifecycle --------------------------------------------------------- */

esp_err_t it8951e_reset(it8951e_handle_t handle) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null arg");

    if (handle->cfg.reset_io != GPIO_NUM_NC) {
        gpio_set_level(handle->cfg.reset_io, 0);
        esp_rom_delay_us(RESET_LOW_US);
        gpio_set_level(handle->cfg.reset_io, 1);
        vTaskDelay(pdMS_TO_TICKS(RESET_HIGH_MS));
    }

    /* Boot may pull HRDY low; wait for it. */
    esp_err_t err = wait_hrdy(handle, HRDY_TIMEOUT_MS);
    if (err != ESP_OK) return err;

    err = pkt_write_cmd(handle, CMD_SYS_RUN);
    if (err != ESP_OK) return err;

    /* Enable packed-write mode so LD_IMG_AREA pixel transfers don't need
     * extra preambles between words. */
    uint16_t i80cpcr = 0;
    err = reg_read(handle, REG_I80CPCR, &i80cpcr);
    if (err == ESP_OK) err = reg_write(handle, REG_I80CPCR, i80cpcr | 0x0001);
    if (err != ESP_OK) return err;

    /* Re-fetch panel info after reset (addresses can change after reflash). */
    handle->info_valid = false;
    err = it8951e_get_panel_info(handle, &handle->info);
    if (err != ESP_OK) return err;

    if (handle->cfg.vcom_v != 0.0f) {
        err = it8951e_set_vcom(handle, handle->cfg.vcom_v);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

esp_err_t it8951e_create(const it8951e_config_t *config, it8951e_handle_t *out_handle) {
    ESP_RETURN_ON_FALSE(config && out_handle, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(GPIO_IS_VALID_GPIO(config->cs_io), ESP_ERR_INVALID_ARG, TAG, "bad cs_io");
    ESP_RETURN_ON_FALSE(GPIO_IS_VALID_GPIO(config->busy_io), ESP_ERR_INVALID_ARG, TAG, "bad busy_io");

    it8951e_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->cfg = *config;

    /* GPIO setup */
    gpio_config_t in = {
        .pin_bit_mask = 1ULL << config->busy_io,
        .mode         = GPIO_MODE_INPUT,
        .pull_up_en   = GPIO_PULLUP_ENABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&in);
    if (err != ESP_OK) goto fail_free;

    if (config->reset_io != GPIO_NUM_NC) {
        gpio_config_t out = {
            .pin_bit_mask = 1ULL << config->reset_io,
            .mode         = GPIO_MODE_OUTPUT,
            .pull_up_en   = GPIO_PULLUP_DISABLE,
            .pull_down_en = GPIO_PULLDOWN_DISABLE,
            .intr_type    = GPIO_INTR_DISABLE,
        };
        err = gpio_config(&out);
        if (err != ESP_OK) goto fail_free;
        gpio_set_level(config->reset_io, 1);
    }

    /* CS is driven manually (cs_set): the read device needs a separate clock,
     * which means two spi_bus devices sharing this one CS pin — but the GPIO
     * matrix routes only one device's CS signal to a pad, so a driver-managed CS
     * would leave the other device unable to assert it. */
    gpio_config_t cs = {
        .pin_bit_mask = 1ULL << config->cs_io,
        .mode         = GPIO_MODE_OUTPUT,
        .pull_up_en   = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    err = gpio_config(&cs);
    if (err != ESP_OK) goto fail_free;
    gpio_set_level(config->cs_io, 1);

    /* Add to the (pre-initialized) SPI bus. The bus is shared with other
     * devices (e.g. microSD); per-transfer locking is via
     * spi_device_acquire_bus(). */
    spi_device_interface_config_t dev_cfg = {
        .clock_speed_hz = config->clock_hz > 0 ? config->clock_hz : IT8951E_SPI_DEFAULT_HZ,
        .mode           = IT8951E_SPI_MODE,
        .spics_io_num   = GPIO_NUM_NC,
        .queue_size     = 1,
        .flags          = 0,
    };
    err = spi_bus_add_device(config->spi_host, &dev_cfg, &dev->spi);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_add_device: %s", esp_err_to_name(err));
        goto fail_free;
    }

    /* Second device on the same bus, clocked slower for register reads. */
    spi_device_interface_config_t rd_cfg = dev_cfg;
    rd_cfg.clock_speed_hz = config->read_clock_hz > 0
                                ? config->read_clock_hz : IT8951E_SPI_READ_DEFAULT_HZ;
    err = spi_bus_add_device(config->spi_host, &rd_cfg, &dev->spi_rd);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_add_device (read): %s", esp_err_to_name(err));
        goto fail_spi;
    }

    err = it8951e_reset(dev);
    if (err != ESP_OK) goto fail_spi_rd;

    *out_handle = dev;
    return ESP_OK;

fail_spi_rd:
    spi_bus_remove_device(dev->spi_rd);
fail_spi:
    spi_bus_remove_device(dev->spi);
fail_free:
    free(dev);
    return err;
}

esp_err_t it8951e_destroy(it8951e_handle_t handle) {
    if (!handle) return ESP_OK;
    if (handle->spi_rd) spi_bus_remove_device(handle->spi_rd);
    if (handle->spi) spi_bus_remove_device(handle->spi);
    free(handle);
    return ESP_OK;
}
