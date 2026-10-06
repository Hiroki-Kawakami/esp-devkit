/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "audf_codec.h"

#ifdef __cplusplus
extern "C" {
#endif

/* AAC-LC, mono or stereo, S16 PCM, with optional SBR and parametric stereo. */
typedef enum {
    AUDF_AAC_HE_OFF = 0,   /*!< core only: an HE-AAC stream plays at its core rate, without the high band */
    AUDF_AAC_HE_V1,        /*!< SBR; a PS stream plays as mono */
    AUDF_AAC_HE_V2,        /*!< SBR and parametric stereo */
} audf_aac_he_t;

typedef struct {
    const uint8_t *asc;        /*!< AudioSpecificConfig, or the first frame when adts */
    size_t         asc_len;
    bool           adts;       /*!< every frame starts with an ADTS header */
    audf_aac_he_t  he;
    uint32_t       alloc_caps; /*!< heap_caps_* for every allocation; 0 = malloc */
    uint32_t       scratch_caps; /*!< for the 12 KB work buffer every stage touches; 0 or
                                      out of memory = alloc_caps */
    uint32_t     (*clock)(void); /*!< cycle counter for audf_aac_take_profile; NULL = off */
} audf_aac_config_t;

typedef enum {
    AUDF_AAC_PROF_CORE = 0,
    AUDF_AAC_PROF_FILTERBANK,
    AUDF_AAC_PROF_QMF_ANALYSIS,
    AUDF_AAC_PROF_SBR,
    AUDF_AAC_PROF_PS,
    AUDF_AAC_PROF_QMF_SYNTHESIS,
    AUDF_AAC_PROF_COUNT,
} audf_aac_prof_t;

/* The output rate and channel count are fixed here. With SBR enabled, a core
 * rate of 24 kHz or less is decoded at twice the rate even when the stream
 * never carries SBR data, and V2 turns a mono core into stereo. */
esp_err_t audf_aac_decoder_create(const audf_aac_config_t *config, audf_decoder_t **out);
uint32_t  audf_aac_decoder_rate(const audf_decoder_t *dec);

/* Clock cycles spent per stage since the last call; false without a clock. */
bool audf_aac_take_profile(audf_decoder_t *dec, uint64_t out[AUDF_AAC_PROF_COUNT]);

/* Runs the SIMD kernels against their C versions on random input; returns the
 * number of mismatching calls (-1 without memory), always 0 where there is no
 * SIMD version. */
int audf_aac_kernel_selftest(uint32_t seed, int iterations);

/* Length of the ADTS frame at data, 0 when data does not start with one. */
size_t audf_aac_adts_frame_len(const uint8_t *data, size_t len);

#ifdef __cplusplus
}
#endif
