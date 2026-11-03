/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The part of libs/harness's API the development benches use, answered
 * through harness_rs (check_c_command).
 */

#pragma once

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef bool (*harness_cmd_fn)(int argc, const char *const *argv, void *user);
bool harness_register(const char *name, harness_cmd_fn fn, void *user);
void harness_reply(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

#ifdef __cplusplus
}
#endif
