/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stddef.h>

/* stdin and stdout are macros, so these stay in C. */
void harness_rs_console_init(void);
int harness_rs_console_getc(void);
void harness_rs_console_write(const void *data, size_t len);
