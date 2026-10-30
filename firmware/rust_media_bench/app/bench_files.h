/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stddef.h>

/* Serves `data`, which stays the caller's, as a read-only file. Returns its
 * NUL-terminated path, NULL on failure. */
const char *bench_file_add(const char *name, const void *data, size_t size);
