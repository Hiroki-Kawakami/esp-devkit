/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * IT8951E e-paper TCON driver (SPI mode), as used by the ESP32 M5Paper.
 * Ported from the m5paper-bsp reference. The bsp_display_t provider that wraps
 * this handle into the BSP display vtable lives in it8951e_epd.{c,h}.
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>
#include "esp_err.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ----------------------------------------------------------------------------
 * IT8951E e-paper TCON driver (SPI mode).
 *
 * The driver does NOT own the SPI bus — the caller initializes the bus via
 * spi_bus_initialize() and passes the host. The driver only attaches itself
 * with spi_bus_add_device(), so other devices (e.g. microSD) can share the
 * same host. The driver acquires the bus around each transfer with
 * spi_device_acquire_bus() so concurrent users are safe as long as they all
 * go through spi_master. That bracket is per-packet, so on a shared bus an SD
 * transaction can interleave between the packets of one load/display sequence;
 * wrap such a sequence in it8951e_bus_acquire()/it8951e_bus_release() to hold
 * the bus across it (release before the long wait_idle so the bus stays free
 * while the panel physically refreshes).
 * -------------------------------------------------------------------------- */

typedef struct it8951e_dev *it8951e_handle_t;

/* Default SPI parameters for IT8951E. Datasheet allows up to ~24 MHz; M5Paper
 * is typically driven around 10 MHz. Register reads (GET_DEV_INFO, the LUTAFSR
 * busy poll) are unreliable at the write clock and need a slower read clock. */
#define IT8951E_SPI_MODE            0
#define IT8951E_SPI_DEFAULT_HZ      (10 * 1000 * 1000)
#define IT8951E_SPI_READ_DEFAULT_HZ (2 * 1000 * 1000)

/* Waveform / update modes. Names match the IT8951E waveform LUT slots used by
 * the M5Paper firmware. Higher quality = slower; 2-level modes are fast. */
typedef enum {
    IT8951E_MODE_INIT = 0,  /* full-screen clear to white (slow, flashes)   */
    IT8951E_MODE_DU   = 1,  /* 2-level direct update, fast                  */
    IT8951E_MODE_GC16 = 2,  /* 16 grayscales, highest quality, slow         */
    IT8951E_MODE_GL16 = 3,  /* 16 grayscales, fewer flashes than GC16       */
    IT8951E_MODE_GLR16 = 4,
    IT8951E_MODE_GLD16 = 5,
    IT8951E_MODE_A2   = 6,  /* 2-level animation, fastest                   */
} it8951e_mode_t;

/* Bits per pixel for image upload. The panel is 16-grayscale, so 4bpp is the
 * native format. 8bpp uploads are accepted and downsampled by the TCON. */
typedef enum {
    IT8951E_BPP_2 = 0,
    IT8951E_BPP_3 = 1,
    IT8951E_BPP_4 = 2,
    IT8951E_BPP_8 = 3,
} it8951e_bpp_t;

/* Pixel rotation applied while loading the image into TCON memory. */
typedef enum {
    IT8951E_ROT_0   = 0,
    IT8951E_ROT_90  = 1,
    IT8951E_ROT_180 = 2,
    IT8951E_ROT_270 = 3,
} it8951e_rotation_t;

/* Endianness of pixel words sent over SPI. */
typedef enum {
    IT8951E_ENDIAN_LITTLE = 0,
    IT8951E_ENDIAN_BIG    = 1,
} it8951e_endian_t;

typedef struct {
    spi_host_device_t spi_host;   /* SPI host the bus was initialized on    */
    gpio_num_t        cs_io;      /* CS for this device                     */
    gpio_num_t        busy_io;    /* HRDY input, HIGH = TCON ready          */
    gpio_num_t        reset_io;   /* RESET output, active LOW; GPIO_NUM_NC to skip */
    int               clock_hz;   /* write clock; 0 -> IT8951E_SPI_DEFAULT_HZ */
    int               read_clock_hz; /* register-read clock; 0 -> IT8951E_SPI_READ_DEFAULT_HZ */
    float             vcom_v;     /* desired VCOM in volts (e.g. -2.30f); 0 -> leave as-is */
} it8951e_config_t;

typedef struct {
    uint16_t panel_w;
    uint16_t panel_h;
    uint16_t mem_addr_l;          /* image buffer base in TCON SDRAM (LSW)  */
    uint16_t mem_addr_h;          /* image buffer base in TCON SDRAM (MSW)  */
    char     fw_version[16];
    char     lut_version[16];
} it8951e_panel_info_t;

/* Region in panel pixels. */
typedef struct {
    uint16_t x;
    uint16_t y;
    uint16_t w;
    uint16_t h;
} it8951e_area_t;

/* Pixel buffer description for upload. */
typedef struct {
    it8951e_bpp_t      bpp;
    it8951e_rotation_t rotation;
    it8951e_endian_t   endian;
    const void        *data;      /* tightly packed per `bpp`               */
    size_t             data_size; /* bytes; must match (w*h*bpp_bits)/8     */
} it8951e_image_t;

/* ----------------------------------------------------------------------------
 * Lifecycle
 * -------------------------------------------------------------------------- */

esp_err_t it8951e_create(const it8951e_config_t *config, it8951e_handle_t *out_handle);
esp_err_t it8951e_destroy(it8951e_handle_t handle);

/* Hold the shared SPI bus across several packets (e.g. a whole load+display)
 * so no other device on the bus can interleave. Reentrant-safe: nested driver
 * packets see the bus already held and skip their own acquire. Release before
 * any long idle wait to keep the bus free for other devices. */
esp_err_t it8951e_bus_acquire(it8951e_handle_t handle);
esp_err_t it8951e_bus_release(it8951e_handle_t handle);

/* Hardware reset via RESET pin, then re-run SYS_RUN / VCOM setup. */
esp_err_t it8951e_reset(it8951e_handle_t handle);

esp_err_t it8951e_get_panel_info(it8951e_handle_t handle, it8951e_panel_info_t *out_info);

/* ----------------------------------------------------------------------------
 * Power management
 * -------------------------------------------------------------------------- */

esp_err_t it8951e_sys_run(it8951e_handle_t handle);
esp_err_t it8951e_standby(it8951e_handle_t handle);
esp_err_t it8951e_sleep(it8951e_handle_t handle);

/* ----------------------------------------------------------------------------
 * VCOM
 * -------------------------------------------------------------------------- */

esp_err_t it8951e_set_vcom(it8951e_handle_t handle, float volts);
esp_err_t it8951e_get_vcom(it8951e_handle_t handle, float *out_volts);

/* ----------------------------------------------------------------------------
 * Drawing
 *
 * Typical LVGL flush sequence:
 *   it8951e_load_image(h, &area, &img);
 *   it8951e_display(h, &area, IT8951E_MODE_GL16);
 *
 * Or as a single call (the driver handles LD_IMG_AREA + DPY_BUF_AREA):
 *   it8951e_flush(h, &area, &img, IT8951E_MODE_GL16);
 *
 * `it8951e_flush` is the path intended for LVGL's flush_cb — it returns
 * promptly after queueing the display command. Pair with it8951e_wait_idle()
 * before calling lv_display_flush_ready().
 *
 * Coordinate constraints: x and w must be aligned to (8 / bpp_bits) — for
 * 4bpp that means multiples of 2 — otherwise IT8951E_ERR_ALIGNMENT.
 * -------------------------------------------------------------------------- */

esp_err_t it8951e_load_image(it8951e_handle_t handle,
                             const it8951e_area_t *area,
                             const it8951e_image_t *image);

esp_err_t it8951e_display(it8951e_handle_t handle,
                          const it8951e_area_t *area,
                          it8951e_mode_t mode);

esp_err_t it8951e_flush(it8951e_handle_t handle,
                        const it8951e_area_t *area,
                        const it8951e_image_t *image,
                        it8951e_mode_t mode);

/* Full-screen clear to white using INIT waveform. */
esp_err_t it8951e_clear(it8951e_handle_t handle);

/* Block (with timeout) until the display engine is idle, i.e. HRDY high and
 * LUTAFSR == 0. `timeout_ms = 0` -> non-blocking poll; UINT32_MAX -> wait
 * forever. */
esp_err_t it8951e_wait_idle(it8951e_handle_t handle, uint32_t timeout_ms);

/* ----------------------------------------------------------------------------
 * Low-level register access (debug / advanced)
 * -------------------------------------------------------------------------- */

esp_err_t it8951e_read_reg(it8951e_handle_t handle, uint16_t reg, uint16_t *out_value);
esp_err_t it8951e_write_reg(it8951e_handle_t handle, uint16_t reg, uint16_t value);

#ifdef __cplusplus
}
#endif
