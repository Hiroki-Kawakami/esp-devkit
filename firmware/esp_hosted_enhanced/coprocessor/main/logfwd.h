/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Copies the log output into a ring buffer and forwards it on the log channel
 * while the host has it enabled. The newest lines win when it fills up.
 */

#pragma once

#include <stdbool.h>

void logfwd_init(void);
void logfwd_enable(bool enable);
