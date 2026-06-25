/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 Panel (Display + Touch)
 */

#pragma once
#include "bsp_display.h"
#include "bsp_touch.h"

esp_err_t paper_s3_panel_init(uint8_t epd_task_priority, int epd_task_affinity);
