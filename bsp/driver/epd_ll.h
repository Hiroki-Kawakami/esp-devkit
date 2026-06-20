/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Low-level driver for direct-drive (TCON-less) EPDs that take an
 * 8-bit parallel source bus + per-line CKV/SPV/LE control. Built on
 * top of ESP-IDF's i80 LCD driver, so it requires SOC_LCD_I80_SUPPORTED.
 *
 * Pin roles map to standard panel naming:
 *   STH (sph) -- horizontal start pulse, wired to i80 CS
 *   CL  (cl)  -- pixel clock,            wired to i80 WR (PCLK)
 *   DBn (data)-- source driver data bus  (i80 D0..D7)
 *   CKV       -- vertical (gate) clock,  manually toggled per scanline
 *   SPV       -- vertical start pulse,   pulsed once per frame
 *   LE        -- source latch enable,    pulsed once per scanline
 *
 * Per-scanline transfer is `line_bytes + line_padding` total. The padding
 * bytes are clocked out the same way as data; the panel ignores them once
 * the latch fires, but they keep CKV asserted long enough for the gate
 * driver to settle. Caller-supplied scanline buffers must be allocated
 * in DMA-capable RAM and pre-cleared in the padding region.
 */
typedef struct {
    int data_pins[8];   /* DB0..DB7                                   */
    int sph_pin;        /* STH (horizontal start pulse, i80 CS)       */
    int cl_pin;         /* PCLK (pixel clock, i80 WR)                 */
    int ckv_pin;        /* CKV (vertical / gate clock)                */
    int spv_pin;        /* SPV (vertical start pulse)                 */
    int le_pin;         /* LE  (source latch enable)                  */
    /* The i80 driver insists on a valid D/C pin even for EPD buses
     * that don't have one. Pass any GPIO you control (e.g. the panel
     * PWR pin) -- the bus will route its D/C signal to it during init,
     * then this driver hands it back as a plain GPIO output before
     * returning, so the caller can use it normally afterward. */
    int dc_dummy_pin;
    uint32_t pclk_hz;   /* i80 pixel clock                            */
    uint16_t line_bytes;   /* scanline data length, in bytes          */
    uint8_t  line_padding; /* extra trailing bytes per scanline tx    */
} epd_ll_config_t;

esp_err_t epd_ll_init(const epd_ll_config_t *cfg);

/* Total scanline transfer size, in bytes: line_bytes + line_padding. */
size_t    epd_ll_line_buf_size(void);

/* Pulse SPV/CKV to scan the gate driver back to the top row.        */
void      epd_ll_frame_begin(void);

/* Wait for the last in-flight scanline and park CKV/LE.             */
void      epd_ll_frame_end(void);

/* Queue one scanline. Blocks until any previous transfer completes,
 * so a single DMA buffer is safe to reuse between calls. The buffer
 * passed here remains in use by DMA until the *next* call (or the
 * matching epd_ll_frame_end()), so callers that want to pipeline
 * should ping-pong two buffers. */
void      epd_ll_write_line(const uint8_t *data);

#ifdef __cplusplus
}
#endif
