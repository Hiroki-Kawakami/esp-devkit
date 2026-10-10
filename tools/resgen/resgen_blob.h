/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stddef.h>
#include <stdint.h>

// Maps `partition` (reads the file at `path` on the simulator) and aborts unless
// it starts with `header`. The mapping is never released.
const uint8_t *resgen_blob_map(const char *partition, const char *path,
                               const uint8_t *header, size_t header_size, size_t size);

void resgen_resources_init(void);
