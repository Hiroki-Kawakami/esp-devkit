/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MEDF_ALAC_COOKIE_BYTES 24

/* The ALACSpecificConfig in a magic cookie, behind any `frma` and `alac` atom
 * headers or the version and flags of an MP4 `alac` box; NULL without one. */
const uint8_t *medf_alac_find_cookie(const uint8_t *data, size_t size);
void medf_alac_cookie_format(const uint8_t *cookie, uint32_t *sample_rate, uint8_t *channels,
                             uint8_t *bits_per_sample);

#ifdef __cplusplus
}
#endif
