/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Recovery entry: the host holds IO2 low across a reset to ask for safe mode.
 * The 3rd such boot in a row without a completed handshake switches to the
 * other OTA slot. Boots are counted in the last flash sector when no partition
 * claims it; the running slot itself cannot be written.
 */

#pragma once

#include <stdbool.h>

/* Call first in app_main. Returns true for safe mode; may not return. */
bool recovery_check(void);

/* The host reached safe mode: forget the pending boots. */
void recovery_clear(void);
