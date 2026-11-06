/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "clock.h"

#include "esp_private/periph_ctrl.h"
#include "hal/jpeg_ll.h"
#include "hal/ppa_ll.h"

void media_engine_jpeg_clock(bool on) {
    PERIPH_RCC_ATOMIC() {
        jpeg_ll_enable_bus_clock(on);
        if (on) jpeg_ll_reset_module_register();
    }
}

void media_engine_ppa_clock(bool on) {
    PERIPH_RCC_ATOMIC() {
        ppa_ll_enable_bus_clock(on);
        if (on) ppa_ll_reset_register();
    }
}
