/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * CoreS3 audio provider: AW88298 mono Class-D amp (speaker output over I2S1)
 * with the AW9523B P0_2 amp-enable line, wired into a bsp_audio_t
 * (caps PCM | SPEAKER). Created by core_s3.c and registered with
 * bsp_audio_set_active(), which owns the click-free bring-up.
 */

#pragma once
#include "bsp_audio.h"
#include "driver/i2c_master.h"
#include "aw9523.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    aw9523_t io_expander;   /*!< AW9523B (P0_2 = amp enable) */
} core_s3_audio_config_t;

esp_err_t core_s3_audio_create(const core_s3_audio_config_t *config, bsp_audio_t **out_audio);

#ifdef __cplusplus
}
#endif
