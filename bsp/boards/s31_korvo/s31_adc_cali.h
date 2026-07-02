/*
 * SPDX-License-Identifier: Apache-2.0
 * SPDX-FileCopyrightText: 2026 Espressif Systems (Shanghai) CO LTD
 *
 * ESP32-S31 software ADC calibration. Ported from esp-dev-kits' esp32_s31_korvo
 * BSP because the standard esp_adc calibration schemes don't support ADC1 on
 * this chip yet. Runs the analog-side calibration sweep once; the raw->mV
 * mapping is a linear fit determined empirically on this board.
 */

#pragma once
#include "esp_err.h"
#include "esp_adc/adc_oneshot.h"

#ifdef __cplusplus
extern "C" {
#endif

/* One-time ADC1 calibration sweep. The handle must have the channel already
 * configured with the atten you plan to use. */
esp_err_t s31_adc_cali_init(adc_oneshot_unit_handle_t handle,
                            adc_channel_t channel);

/* Linear raw->mV. ctx unused (matches adc_button_raw_to_mv_fn shape). */
esp_err_t s31_adc_cali_raw_to_mv(int raw, int *out_mv, void *ctx);

#ifdef __cplusplus
}
#endif
