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

/* Registers the C++ commands (wifi, ble). */
void check_start(void);

/* Joins the saved network and serves netbench once connected. */
void check_wifi_autoconnect(void);

/* Runs a command registered with harness_register; `reply` gets the body it
 * replied with, empty when none. false for an unknown command or a failure. */
bool check_c_command(int argc, const char *const *argv, char *reply, size_t reply_len);

#ifdef __cplusplus
}
#endif
