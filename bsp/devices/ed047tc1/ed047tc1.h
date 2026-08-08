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
#include "epd_ll.h"

typedef struct {
    int data_pins[8];   /* DB0..DB7 source data bus                  */
    int sph_pin;        /* STH / i80 CS                              */
    int cl_pin;         /* CL  / i80 WR (PCLK)                       */
    int dc_dummy_pin;   /* i80 requires a D/C GPIO; panel has none   */
    epd_ll_control_config_t control;
    uint8_t task_priority;  /* async refresh-task priority               */
    int     task_affinity;  /* core to pin the refresh task to; <0 -> none */
} ed047tc1_config_t;

esp_err_t ed047tc1_epd_create(const ed047tc1_config_t *cfg, bsp_display_t **out_display);
