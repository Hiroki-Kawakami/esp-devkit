/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ED047TC1 grayscale EPD panel descriptor. The framebuffer + waveform engine +
 * i80 bus all live in the epd_ll driver; this just binds the panel-intrinsic
 * values (geometry, timing, waveforms; see ed047tc1.c) to the
 * board's GPIO map (ed047tc1_config_t) and calls epd_ll_create, returning the
 * resulting bsp_display_t provider.
 */

#pragma once
#include "bsp_display.h"

typedef struct {
    int data_pins[8];   /* DB0..DB7 source data bus                  */
    int sph_pin;        /* STH / i80 CS                              */
    int cl_pin;         /* CL  / i80 WR (PCLK)                       */
    int ckv_pin;        /* CKV (vertical / gate clock)               */
    int spv_pin;        /* SPV (vertical start pulse)                */
    int le_pin;         /* LE  (source latch enable)                 */
    int oe_pin;         /* OE  (panel output enable, power seq)      */
    int pwr_pin;        /* PWR (EPD DC/DC enable, power seq); also    */
                        /* lent to the i80 bus as its dummy D/C pin   */
    uint8_t task_priority;  /* async refresh-task priority               */
    int     task_affinity;  /* core to pin the refresh task to; <0 -> none */
} ed047tc1_config_t;

esp_err_t ed047tc1_epd_create(const ed047tc1_config_t *cfg, bsp_display_t **out_display);
