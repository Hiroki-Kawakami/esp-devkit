/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Waveform-tuning backdoor for the epd_tweak firmware: swap a live panel's
 * waveform LUT without a rebuild/reflash. Compiled (and includable) only when
 * the build sets EPD_LL_TWEAK=1 -- production firmware has neither the symbol
 * nor the header on its include path.
 */
#pragma once
#include "epd_ll.h"

#if !defined(EPD_LL_TWEAK) || !EPD_LL_TWEAK
#error "epd_ll_tweak.h is only available in EPD_LL_TWEAK=1 builds (epd_tweak firmware)"
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Rebind `waveform` of the most recently created epd_ll panel. Validates like
 * epd_ll_create and BLOCKS until every in-flight pixel retires (waveforms are
 * never interrupted). `lut` is caller-owned: it must stay alive and unmodified
 * until replaced by a later call (double-buffer on the caller side). */
esp_err_t epd_ll_tweak_set_waveform_lut(epd_ll_waveform_t waveform,
                                        const uint32_t (*lut)[16], size_t steps);

/* Currently bound table (NULL / 0 when the slot is empty). */
esp_err_t epd_ll_tweak_get_waveform_lut(epd_ll_waveform_t waveform,
                                        epd_ll_lut_t *out_lut, size_t *out_steps);

#ifdef __cplusplus
}
#endif
