/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * IT8951E EPD display provider: wraps the low-level it8951e SPI TCON driver
 * (it8951e.h) into a BSP display vtable (bsp_display_t), mirroring the role
 * ed047tc1.c plays for the M5PaperS3 panel. The board passes the same
 * it8951e_config_t it would hand to it8951e_create; this owns the handle, keeps
 * a panel-sized L8 GRAM, and implements draw_bitmap / set_epd_mode / refresh on
 * top of it8951e_load_image + it8951e_display.
 */

#pragma once
#include "bsp_display.h"
#include "it8951e.h"

esp_err_t it8951e_epd_create(const it8951e_config_t *cfg, bsp_display_t **out_display);
