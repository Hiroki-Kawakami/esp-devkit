/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The PSRAM region the simulator host shares; false when not run under it, in
 * which case MALLOC_CAP_SPIRAM comes from the host heap. */
bool sim_psram_region(void **base, size_t *size);

#ifdef __cplusplus
}
#endif
