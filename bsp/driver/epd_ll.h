/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"
#include "bsp_display.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Driver for direct-drive (TCON-less) grayscale EPDs that take an 8-bit parallel
 * source bus + per-line CKV/SPV/LE control, built on ESP-IDF's i80 LCD driver
 * (requires SOC_LCD_I80_SUPPORTED). This is the whole engine, not just the bus:
 * it owns the framebuffer, runs a time-axis waveform LUT with differential drive
 * synchronously (refresh blocks until the panel settles), and exposes a
 * bsp_display_t provider. Everything
 * panel-specific (geometry, scanline format, power-rail pins, waveform LUTs)
 * comes in through epd_ll_config_t; a per-panel descriptor (e.g. ed047tc1) just
 * fills this in and calls epd_ll_create. Re-tailored per SoC/bus, not abstracted.
 *
 * Pin roles map to standard panel naming:
 *   STH (sph) -- horizontal start pulse, wired to i80 CS
 *   CL  (cl)  -- pixel clock,            wired to i80 WR (PCLK)
 *   DBn (data)-- source driver data bus  (i80 D0..D7)
 *   CKV       -- vertical (gate) clock,  toggled per scanline
 *   SPV       -- vertical start pulse,   pulsed once per frame
 *   LE        -- source latch enable,    pulsed once per scanline
 *   OE / PWR  -- panel output-enable / DC-DC enable, sequenced around each refresh
 *
 * Per-scanline transfer is line_bytes + line_padding total. The padding bytes are
 * clocked out the same way as data; the panel ignores them once the latch fires,
 * but they keep CKV asserted long enough for the gate driver to settle.
 *
 * Waveform LUT: one uint32 per frame; each of the 16 gray columns gets a 2-bit
 * action (0 = hold, 1 = drive to black, 2 = drive to white). Replay length is the
 * step count. The scanline format is fixed at 2 bits/pixel (4 px/byte, leftmost
 * in the high pair), matching the action encoding.
 */
typedef struct {
    int data_pins[8];   /* DB0..DB7                                   */
    int sph_pin;        /* STH (horizontal start pulse, i80 CS)       */
    int cl_pin;         /* CL  (pixel clock, i80 WR / PCLK)           */
    int ckv_pin;        /* CKV (vertical / gate clock)                */
    int spv_pin;        /* SPV (vertical start pulse)                 */
    int le_pin;         /* LE  (source latch enable)                  */
    int oe_pin;         /* OE  (panel output enable, power seq)       */
    int pwr_pin;        /* PWR (DC-DC enable, power seq)              */
    /* The i80 driver insists on a valid D/C pin even for EPD buses that don't
     * have one. Pass any GPIO you control (e.g. the PWR pin) -- the bus routes
     * its D/C there during init, then it's reclaimed as a plain output. */
    int dc_dummy_pin;
    uint32_t pclk_hz;       /* i80 pixel clock                        */
    int      width;         /* panel width, px                        */
    int      height;        /* panel height, px                       */
    uint16_t line_bytes;    /* scanline data length, in bytes         */
    uint8_t  line_padding;  /* extra trailing bytes per scanline tx   */
    /* Time-axis waveform LUT for a refresh mode (the FULL flag is stripped before
     * the call): returns the LUT and writes its frame count to *steps, or returns
     * NULL / *steps = 0 when the panel has no waveform for that mode. Adding a
     * refresh mode is just another case here -- no config churn. Must include a
     * BSP_EPD_MODE_CLEAR waveform: the bring-up white baseline runs it as
     * CLEAR_FULL. Returned tables are caller-owned and must outlive the display. */
    const uint32_t *(*get_waveform_lut)(bsp_epd_mode_t mode, size_t *steps);
    uint8_t  task_priority;     /* reserved (refresh is currently synchronous)  */
    int      task_affinity;     /* reserved (refresh is currently synchronous)  */
} epd_ll_config_t;

/* Bring up the panel -- allocate the framebuffers, init the i80 bus, clear to a
 * known white baseline -- and hand back a bsp_display_t provider. */
esp_err_t epd_ll_create(const epd_ll_config_t *cfg, bsp_display_t **out_display);

#ifdef __cplusplus
}
#endif
